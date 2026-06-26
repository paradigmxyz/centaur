import { describe, expect, test } from "bun:test";
import {
  decideMerge,
  evaluateCi,
  handleCiEvent,
  handleReviewEvent,
  isOwnedPr,
  type CiCheck,
  type PrManagerContext,
} from "../src/pr-manager";

function makeState() {
  const values = new Map<string, unknown>();
  return {
    async get(key: string) {
      return values.get(key);
    },
    async set(key: string, value: unknown) {
      values.set(key, value);
    },
    async setIfNotExists(key: string, value: unknown) {
      if (values.has(key)) return false;
      values.set(key, value);
      return true;
    },
    async delete(key: string) {
      values.delete(key);
    },
  };
}

function prPayload(input: {
  assignees?: { login: string }[];
  headRepoFullName: string;
  headSha?: string;
  mergeableState?: string;
  number?: number;
}) {
  return {
    assignees: input.assignees ?? [{ login: "centaur-bot" }],
    draft: false,
    head: {
      ref: "feature",
      repo: { full_name: input.headRepoFullName },
      sha: input.headSha ?? "abc123",
    },
    labels: [],
    mergeable_state: input.mergeableState ?? "clean",
    merged: false,
    number: input.number ?? 7,
    state: "open",
    title: "Test PR",
  };
}

describe("evaluateCi", () => {
  test("not settled while any check is in progress", () => {
    const checks: CiCheck[] = [
      { name: "build", status: "completed", conclusion: "success" },
      { name: "test", status: "in_progress", conclusion: null },
    ];
    expect(evaluateCi(checks, [])).toMatchObject({ settled: false });
  });

  test("settled + green when all checks succeed", () => {
    const checks: CiCheck[] = [
      { name: "build", status: "completed", conclusion: "success" },
      { name: "test", status: "completed", conclusion: "skipped" },
    ];
    expect(evaluateCi(checks, [])).toEqual({
      settled: true,
      failed: false,
      failingNames: [],
    });
  });

  test("settled + red, collecting failing names from checks and statuses", () => {
    const checks: CiCheck[] = [
      { name: "build", status: "completed", conclusion: "success" },
      { name: "lint", status: "completed", conclusion: "failure" },
      { name: "e2e", status: "completed", conclusion: "timed_out" },
    ];
    const result = evaluateCi(checks, [
      { state: "success", context: "coverage" },
      { state: "error", context: "deploy-preview" },
    ]);
    expect(result.settled).toBe(true);
    expect(result.failed).toBe(true);
    expect(result.failingNames.sort()).toEqual(["deploy-preview", "e2e", "lint"]);
  });

  test("pending legacy status keeps it unsettled", () => {
    const result = evaluateCi(
      [{ name: "build", status: "completed", conclusion: "success" }],
      [{ state: "pending", context: "deploy" }],
    );
    expect(result.settled).toBe(false);
  });
});

describe("isOwnedPr", () => {
  test("owned when the bot is an assignee (case-insensitive)", () => {
    expect(
      isOwnedPr({
        assignees: ["someone-else", "Centaur-Bot"],
        userName: "centaur-bot",
      }),
    ).toBe(true);
  });

  test("not owned when the bot is not an assignee", () => {
    expect(
      isOwnedPr({
        assignees: ["someone-else"],
        userName: "centaur-bot",
      }),
    ).toBe(false);
  });

  test("not owned when there are no assignees", () => {
    expect(isOwnedPr({ assignees: [], userName: "centaur-bot" })).toBe(false);
  });
});

describe("decideMerge", () => {
  const base = {
    autoMerge: true,
    draft: false,
    holdLabel: "do-not-merge",
    labels: [] as string[],
    merged: false,
    mergeableState: "clean",
    state: "open",
  };

  test("merges a clean, open, non-draft PR", () => {
    expect(decideMerge(base)).toBe("merge");
  });

  test("respects the global disable switch", () => {
    expect(decideMerge({ ...base, autoMerge: false })).toBe("skip_disabled");
  });

  test("respects the per-PR hold label (case-insensitive)", () => {
    expect(decideMerge({ ...base, labels: ["Do-Not-Merge"] })).toBe("skip_hold");
  });

  test("does not merge drafts or closed/merged PRs", () => {
    expect(decideMerge({ ...base, draft: true })).toBe("skip_draft");
    expect(decideMerge({ ...base, merged: true })).toBe("skip_closed");
    expect(decideMerge({ ...base, state: "closed" })).toBe("skip_closed");
  });

  test("routes dirty -> conflict and behind -> update", () => {
    expect(decideMerge({ ...base, mergeableState: "dirty" })).toBe("resolve_conflict");
    expect(decideMerge({ ...base, mergeableState: "behind" })).toBe("update_branch");
  });

  test("waits on blocked/unstable/unknown states", () => {
    expect(decideMerge({ ...base, mergeableState: "blocked" })).toBe("wait");
    expect(decideMerge({ ...base, mergeableState: "unstable" })).toBe("wait");
    expect(decideMerge({ ...base, mergeableState: "unknown" })).toBe("wait");
  });
});

describe("PR management webhooks", () => {
  test("does not delete a base-repo branch after merging a fork PR", async () => {
    let deleteRefCalls = 0;
    let mergeCalls = 0;
    const ctx = {
      octokit: {
        rest: {
          pulls: {
            get: async () => ({
              data: prPayload({ headRepoFullName: "contributor/repo" }),
            }),
            merge: async () => {
              mergeCalls += 1;
              return { data: {} };
            },
          },
          git: {
            deleteRef: async () => {
              deleteRefCalls += 1;
              return { data: {} };
            },
          },
        },
      },
      options: {
        apiUrl: "http://localhost",
        logger: { debug() {}, warn() {}, error() {}, info() {} },
      },
      state: makeState(),
      userName: "centaur-bot",
    } as unknown as PrManagerContext;

    await handleReviewEvent(
      ctx,
      JSON.stringify({
        action: "submitted",
        repository: { full_name: "base/repo" },
        pull_request: { number: 7 },
        review: { id: 123, state: "approved", user: { login: "reviewer" } },
      }),
    );

    expect(mergeCalls).toBe(1);
    expect(deleteRefCalls).toBe(0);
  });

  test("routes legacy status webhooks through associated PRs", async () => {
    let associatedCommitSha: string | undefined;
    let mergeCalls = 0;
    const ctx = {
      octokit: {
        rest: {
          checks: {
            listForRef: async () => ({ data: { check_runs: [] } }),
          },
          pulls: {
            get: async () => ({
              data: prPayload({
                headRepoFullName: "base/repo",
                headSha: "abc123",
              }),
            }),
            merge: async () => {
              mergeCalls += 1;
              return { data: {} };
            },
          },
          repos: {
            getCombinedStatusForRef: async () => ({
              data: { statuses: [{ state: "success", context: "legacy-ci" }] },
            }),
            listPullRequestsAssociatedWithCommit: async (input: {
              commit_sha: string;
            }) => {
              associatedCommitSha = input.commit_sha;
              return { data: [{ number: 7 }] };
            },
          },
          git: {
            deleteRef: async () => ({ data: {} }),
          },
        },
      },
      options: {
        apiUrl: "http://localhost",
        deleteBranchOnMerge: false,
        logger: { debug() {}, warn() {}, error() {}, info() {} },
      },
      state: makeState(),
      userName: "centaur-bot",
    } as unknown as PrManagerContext;

    await handleCiEvent(
      ctx,
      "status",
      JSON.stringify({
        repository: { full_name: "base/repo" },
        sha: "abc123",
        state: "success",
      }),
    );

    expect(associatedCommitSha).toBe("abc123");
    expect(mergeCalls).toBe(1);
  });
});

const approvedReview = (reviewId: number) =>
  JSON.stringify({
    action: "submitted",
    repository: { full_name: "base/repo" },
    pull_request: { number: 7 },
    review: { id: reviewId, state: "approved", user: { login: "reviewer" } },
  });

const quietLogger = { debug() {}, warn() {}, error() {}, info() {} };

describe("merge claim lifecycle", () => {
  function mergeCtx(merge: () => Promise<unknown>) {
    return {
      octokit: {
        rest: {
          pulls: {
            get: async () => ({
              data: prPayload({ headRepoFullName: "base/repo" }),
            }),
            merge,
          },
          git: { deleteRef: async () => ({ data: {} }) },
        },
      },
      options: {
        apiUrl: "http://localhost",
        deleteBranchOnMerge: false,
        logger: quietLogger,
      },
      state: makeState(),
      userName: "centaur-bot",
    } as unknown as PrManagerContext;
  }

  test("releases the claim when merge fails, so a later event retries", async () => {
    let mergeCalls = 0;
    const ctx = mergeCtx(async () => {
      mergeCalls += 1;
      if (mergeCalls === 1) throw new Error("Base branch was modified");
      return { data: {} };
    });
    await handleReviewEvent(ctx, approvedReview(1));
    await handleReviewEvent(ctx, approvedReview(2));
    expect(mergeCalls).toBe(2);
  });

  test("keeps the claim on success, so the same head sha is not re-merged", async () => {
    let mergeCalls = 0;
    const ctx = mergeCtx(async () => {
      mergeCalls += 1;
      return { data: {} };
    });
    await handleReviewEvent(ctx, approvedReview(1));
    await handleReviewEvent(ctx, approvedReview(2));
    expect(mergeCalls).toBe(1);
  });
});

describe("CI fix counter and escalation", () => {
  const redCheckRun = JSON.stringify({
    repository: { full_name: "base/repo" },
    check_run: { head_sha: "abc123", pull_requests: [{ number: 7 }] },
  });

  function ciCtx(
    state: ReturnType<typeof makeState>,
    comments: string[],
  ): PrManagerContext {
    return {
      octokit: {
        rest: {
          checks: {
            listForRef: async () => ({
              data: {
                check_runs: [
                  { name: "build", status: "completed", conclusion: "failure" },
                ],
              },
            }),
          },
          repos: {
            getCombinedStatusForRef: async () => ({ data: { statuses: [] } }),
            getCommit: async () => ({
              data: { author: { login: "centaur-bot" } },
            }),
          },
          pulls: {
            get: async () => ({
              data: prPayload({ headRepoFullName: "base/repo" }),
            }),
          },
          issues: {
            createComment: async (input: { body: string }) => {
              comments.push(input.body);
              return { data: {} };
            },
          },
        },
      },
      options: {
        apiUrl: "http://localhost",
        ciFixMaxAttempts: 3,
        escalationHandle: "maintainer",
        logger: quietLogger,
        // Non-retryable so the backgrounded fix turn settles off the network.
        fetch: () => Promise.resolve(new Response("no", { status: 400 })),
      },
      state,
      userName: "centaur-bot",
    } as unknown as PrManagerContext;
  }

  test("increments the consecutive-fix counter below the cap", async () => {
    const state = makeState();
    await handleCiEvent(ciCtx(state, []), "check_run", redCheckRun);
    expect(await state.get("centaur-githubbot:pr:base/repo#7")).toMatchObject({
      consecutiveCiFixes: 1,
    });
  });

  test("escalates and fires no fix turn once the cap is reached", async () => {
    const state = makeState();
    await state.set("centaur-githubbot:pr:base/repo#7", {
      consecutiveCiFixes: 3,
    });
    const comments: string[] = [];
    await handleCiEvent(ciCtx(state, comments), "check_run", redCheckRun);
    expect(comments.length).toBe(1);
    expect(comments[0]).toContain("@maintainer");
    // The counter is not bumped past the cap.
    expect(await state.get("centaur-githubbot:pr:base/repo#7")).toMatchObject({
      consecutiveCiFixes: 3,
    });
  });
});
