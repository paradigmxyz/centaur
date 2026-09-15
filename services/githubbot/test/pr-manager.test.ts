import { describe, expect, test } from "bun:test";
import { drainBackgroundWork } from "../src/context";
import {
  decideMerge,
  handleCiEvent,
  handleReviewEvent,
  isOwnedPr,
  type PrManagerContext,
} from "../src/pr-manager";
import { emitWorkflowEvent } from "../src/session-api";
import {
  evaluateCi,
  fetchCiEvaluation,
  isCentaurSkipCheck,
  type CiCheck,
} from "../src/workflow-events";

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

describe("centaur-skip checks", () => {
  function checkRun(input: { conclusion: string; name: string }) {
    return {
      __typename: "CheckRun",
      conclusion: input.conclusion,
      name: input.name,
      startedAt: "2026-08-01T10:00:00Z",
      status: "COMPLETED",
    };
  }

  // As the gate really reports it: the check name is the job id.
  const gate = checkRun({ conclusion: "FAILURE", name: "agent-pr-rules-centaur-skip" });

  function rollupCtx(rollup: {
    state: string;
    nodes: (Record<string, unknown> | null)[];
    counts?: { count: number; state: string }[];
  }) {
    return {
      octokit: {
        graphql: async () => ({
          repository: {
            object: {
              statusCheckRollup: {
                state: rollup.state,
                contexts: {
                  nodes: rollup.nodes,
                  pageInfo: { hasNextPage: false, endCursor: null },
                  checkRunCountsByState: rollup.counts,
                  statusContextCountsByState: rollup.counts ? [] : undefined,
                },
              },
            },
          },
        }),
        // No Actions data: the fallback finds nothing and degrades, which is
        // what the aggregate-fallback case below asserts.
        rest: {
          actions: {
            listWorkflowRunsForRepo: async () => ({ data: { workflow_runs: [] } }),
            listJobsForWorkflowRun: async () => ({ data: { jobs: [] } }),
          },
        },
      },
      options: { logger: quietLogger },
    } as unknown as PrManagerContext;
  }

  test("matches the marker anywhere in the name, case-insensitively", () => {
    for (const name of [
      "agent-pr-rules-centaur-skip",
      "centaur-skip-approvals",
      "Approvals (CENTAUR-SKIP)",
    ]) {
      expect(isCentaurSkipCheck(name)).toBe(true);
    }
  });

  test("leaves an unmarked or absent name alone", () => {
    for (const name of ["agent-pr-rules", "centaur", "skip", ""]) {
      expect(isCentaurSkipCheck(name)).toBe(false);
    }
    expect(isCentaurSkipCheck(null)).toBe(false);
    expect(isCentaurSkipCheck(undefined)).toBe(false);
  });

  test("a marked failure leaves CI green, despite a FAILURE aggregate", async () => {
    // Green here is only possible if the detail wins over the FAILURE aggregate.
    const ctx = rollupCtx({
      state: "FAILURE",
      nodes: [gate, checkRun({ conclusion: "SUCCESS", name: "build" })],
    });
    await expect(fetchCiEvaluation(ctx, "base", "repo", "abc123")).resolves.toEqual({
      failed: false,
      failingNames: [],
      settled: true,
    });
  });

  test("a real failure alongside it still fails, naming only the real one", async () => {
    const ctx = rollupCtx({
      state: "FAILURE",
      nodes: [gate, checkRun({ conclusion: "FAILURE", name: "build" })],
    });
    await expect(fetchCiEvaluation(ctx, "base", "repo", "abc123")).resolves.toEqual({
      failed: true,
      failingNames: ["build"],
      settled: true,
    });
  });

  test("an unreadable detail set with no Actions data falls back to the aggregate", async () => {
    // A null node means contexts are missing, not marked: aggregate is all we have.
    const ctx = rollupCtx({
      state: "FAILURE",
      nodes: [null, checkRun({ conclusion: "SUCCESS", name: "build" })],
      counts: [{ count: 2, state: "COMPLETED" }],
    });
    await expect(fetchCiEvaluation(ctx, "base", "repo", "abc123")).resolves.toEqual({
      failed: true,
      failingNames: [],
      settled: true,
    });
  });
});

// A fine-grained PAT has no Checks permission, so every check node comes back
// null and the rollup alone can't tell a marked check from a real failure.
describe("Actions fallback when check nodes are unreadable", () => {
  function statusCtx(context: string, state: string) {
    return { __typename: "StatusContext", context, createdAt: "2026-08-01T10:00:00Z", state };
  }
  function job(name: string, conclusion: string | null, status = "completed") {
    return { conclusion, name, status };
  }
  function ctxFor(input: {
    nodes: (Record<string, unknown> | null)[];
    checkRuns: number;
    statuses: number;
    actionsJobs: Record<number, ReturnType<typeof job>[]>;
  }) {
    const runIds = Object.keys(input.actionsJobs).map(Number);
    return {
      octokit: {
        graphql: async () => ({
          repository: {
            object: {
              statusCheckRollup: {
                state: "FAILURE",
                contexts: {
                  nodes: input.nodes,
                  pageInfo: { hasNextPage: false, endCursor: null },
                  checkRunCountsByState: [{ count: input.checkRuns, state: "COMPLETED" }],
                  statusContextCountsByState: [{ count: input.statuses, state: "SUCCESS" }],
                },
              },
            },
          },
        }),
        rest: {
          actions: {
            listWorkflowRunsForRepo: async () => ({
              data: { workflow_runs: runIds.map((id) => ({ id })) },
            }),
            listJobsForWorkflowRun: async ({ run_id }: { run_id: number }) => ({
              data: { jobs: input.actionsJobs[run_id] ?? [] },
            }),
          },
        },
      },
      options: { logger: quietLogger },
    } as unknown as PrManagerContext;
  }

  test("reconstructs the checks and drops the marked one", async () => {
    // Exactly what splits-teams reports: 5 null check nodes, Vercel readable.
    const ctx = ctxFor({
      nodes: [null, null, null, null, null, statusCtx("Vercel", "SUCCESS")],
      checkRuns: 5,
      statuses: 1,
      actionsJobs: {
        3: [job("agent-pr-rules-centaur-skip", "failure")],
        2: [job("assign-author", "success")],
        1: [job("lint-and-format", "success"), job("typecheck", "success"), job("test", "success")],
      },
    });
    await expect(fetchCiEvaluation(ctx, "base", "repo", "abc123")).resolves.toEqual({
      failed: false,
      failingNames: [],
      settled: true,
    });
  });

  test("still reports a real failure, and names it", async () => {
    const ctx = ctxFor({
      nodes: [null, null, statusCtx("Vercel", "SUCCESS")],
      checkRuns: 2,
      statuses: 1,
      actionsJobs: {
        2: [job("agent-pr-rules-centaur-skip", "failure")],
        1: [job("typecheck", "failure")],
      },
    });
    await expect(fetchCiEvaluation(ctx, "base", "repo", "abc123")).resolves.toEqual({
      failed: true,
      failingNames: ["typecheck"],
      settled: true,
    });
  });

  test("refuses to trust an incomplete reconstruction", async () => {
    // 3 check runs counted but only 2 are Actions jobs: something else posted
    // one (another App), and it could be the red one. Degrade, don't guess.
    const ctx = ctxFor({
      nodes: [null, null, null, statusCtx("Vercel", "SUCCESS")],
      checkRuns: 3,
      statuses: 1,
      actionsJobs: {
        2: [job("agent-pr-rules-centaur-skip", "failure")],
        1: [job("typecheck", "success")],
      },
    });
    await expect(fetchCiEvaluation(ctx, "base", "repo", "abc123")).resolves.toEqual({
      failed: true,
      failingNames: [],
      settled: true,
    });
  });

  test("refuses when a commit status is unreadable too", async () => {
    // 2 statuses counted, 1 readable: a red status could be hiding.
    const ctx = ctxFor({
      nodes: [null, statusCtx("Vercel", "SUCCESS")],
      checkRuns: 1,
      statuses: 2,
      actionsJobs: { 1: [job("agent-pr-rules-centaur-skip", "failure")] },
    });
    await expect(fetchCiEvaluation(ctx, "base", "repo", "abc123")).resolves.toEqual({
      failed: true,
      failingNames: [],
      settled: true,
    });
  });

  test("a superseded re-run never overwrites the newest job", async () => {
    // Same job name in two runs; only the newest (higher id) counts, so the
    // stale failure must not resurface.
    const ctx = ctxFor({
      nodes: [null, statusCtx("Vercel", "SUCCESS")],
      checkRuns: 1,
      statuses: 1,
      actionsJobs: { 9: [job("typecheck", "success")], 4: [job("typecheck", "failure")] },
    });
    await expect(fetchCiEvaluation(ctx, "base", "repo", "abc123")).resolves.toEqual({
      failed: false,
      failingNames: [],
      settled: true,
    });
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
        graphql: async () => ({
          repository: {
            object: {
              statusCheckRollup: { state: "SUCCESS", contexts: { nodes: [] } },
            },
          },
        }),
        rest: {
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
        graphql: async () => ({
          repository: {
            object: {
              statusCheckRollup: {
                state: "FAILURE",
                contexts: {
                  nodes: [
                    {
                      __typename: "CheckRun",
                      name: "build",
                      status: "COMPLETED",
                      conclusion: "FAILURE",
                    },
                  ],
                },
              },
            },
          },
        }),
        rest: {
          repos: {
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

describe("workflow event emission", () => {
  type EmitCall = {
    url: string;
    body: { event_type?: string; correlation_id?: string; payload?: unknown };
  };

  // The PR is deliberately NOT bot-owned (no assignees): workflow events must
  // emit before the owned-PR gate, and the management path then no-ops, so no
  // merge/turn mocks are needed.
  type RollupStub = {
    state?: string;
    contexts?: (Record<string, unknown> | null)[];
    checkRunCountsByState?: { count: number; state: string }[];
    fail?: boolean;
    pageInfo?: { endCursor?: string | null; hasNextPage: boolean };
    partial?: boolean;
    statusContextCountsByState?: { count: number; state: string }[];
  };

  function emitCtx(
    emits: EmitCall[],
    options?: {
      rollupSequence?: RollupStub[];
      workflowEvents?: boolean;
    },
  ): PrManagerContext {
    const sequence = [...(options?.rollupSequence ?? [{ state: "SUCCESS" }])];
    return {
      octokit: {
        graphql: async () => {
          const next = sequence.length > 1 ? sequence.shift()! : sequence[0]!;
          if (next.fail) throw new Error("403 Forbidden");
          const result = {
            repository: {
              object: {
                statusCheckRollup: {
                  state: next.state ?? "SUCCESS",
                  contexts: {
                    nodes: next.contexts ?? [],
                    pageInfo: next.pageInfo,
                    checkRunCountsByState: next.checkRunCountsByState,
                    statusContextCountsByState: next.statusContextCountsByState,
                  },
                },
              },
            },
          };
          if (next.partial) {
            throw Object.assign(new Error("partial GraphQL result"), { data: result });
          }
          return result;
        },
        rest: {
          repos: {
            listPullRequestsAssociatedWithCommit: async () => ({ data: [] }),
          },
          pulls: {
            get: async () => ({
              data: prPayload({ assignees: [], headRepoFullName: "base/repo" }),
            }),
          },
        },
      },
      options: {
        apiUrl: "http://localhost",
        ciSettleConfirmMs: 0,
        logger: quietLogger,
        workflowEvents: options?.workflowEvents ?? true,
        fetch: (url: RequestInfo | URL, init?: RequestInit) => {
          emits.push({
            url: String(url),
            body: JSON.parse(String(init?.body ?? "{}")),
          });
          return Promise.resolve(new Response('{"ok":true}', { status: 200 }));
        },
      },
      state: makeState(),
      userName: "centaur-bot",
    } as unknown as PrManagerContext;
  }

  const completedCheckRun = JSON.stringify({
    action: "completed",
    repository: { full_name: "base/repo" },
    check_run: {
      head_sha: "abc123",
      name: "build",
      conclusion: "success",
      html_url: "https://example.test/checks/1",
      pull_requests: [{ number: 7 }],
    },
  });

  test("emits ci-completed for a completed check run before ownership gating", async () => {
    const emits: EmitCall[] = [];
    await handleCiEvent(emitCtx(emits), "check_run", completedCheckRun);
    await drainBackgroundWork(1_000);
    expect(emits.length).toBe(1);
    const emit = emits[0]!;
    expect(emit.url).toBe("http://localhost/api/workflows/events");
    expect(emit.body).toEqual({
      event_type: "ci-completed",
      correlation_id: "base/repo:abc123",
      payload: { failed: false, failing: [] },
    });
  });

  test("lowercases correlation ids against repository full_name case drift", async () => {
    const emits: EmitCall[] = [];
    await handleCiEvent(
      emitCtx(emits),
      "check_run",
      JSON.stringify({
        action: "completed",
        repository: { full_name: "Base/Repo" },
        check_run: { head_sha: "ABC123", pull_requests: [{ number: 7 }] },
      }),
    );
    await drainBackgroundWork(1_000);
    expect(emits.length).toBe(1);
    expect(emits[0]!.body.correlation_id).toBe("base/repo:abc123");
  });

  test("emits ci-completed for a terminal legacy status", async () => {
    const emits: EmitCall[] = [];
    await handleCiEvent(
      emitCtx(emits, {
        rollupSequence: [
          {
            state: "FAILURE",
            contexts: [
              { __typename: "StatusContext", context: "deploy", state: "FAILURE" },
            ],
          },
        ],
      }),
      "status",
      JSON.stringify({
        repository: { full_name: "base/repo" },
        sha: "abc123",
        state: "failure",
        context: "deploy",
        target_url: "https://example.test/deploy/1",
      }),
    );
    await drainBackgroundWork(1_000);
    expect(emits.length).toBe(1);
    expect(emits[0]!.body).toEqual({
      event_type: "ci-completed",
      correlation_id: "base/repo:abc123",
      payload: { failed: true, failing: ["deploy"] },
    });
  });

  test("does not emit ci-completed while any check is still running", async () => {
    const emits: EmitCall[] = [];
    await handleCiEvent(
      emitCtx(emits, { rollupSequence: [{ state: "PENDING" }] }),
      "check_run",
      completedCheckRun,
    );
    expect(emits.length).toBe(0);
  });

  test("does not treat a failed aggregate as settled while another check is running", async () => {
    const emits: EmitCall[] = [];
    await handleCiEvent(
      emitCtx(emits, {
        rollupSequence: [
          {
            state: "FAILURE",
            contexts: [
              {
                __typename: "CheckRun",
                conclusion: "FAILURE",
                name: "lint",
                status: "COMPLETED",
              },
              {
                __typename: "CheckRun",
                conclusion: null,
                name: "test",
                status: "IN_PROGRESS",
              },
            ],
          },
        ],
      }),
      "check_run",
      completedCheckRun,
    );
    expect(emits.length).toBe(0);
  });

  test("uses the latest check run when a failed job is rerun successfully", async () => {
    const emits: EmitCall[] = [];
    const workflow = {
      workflowRun: { event: "pull_request", workflow: { name: "CI" } },
    };
    await handleCiEvent(
      emitCtx(emits, {
        rollupSequence: [
          {
            state: "SUCCESS",
            contexts: [
              {
                __typename: "CheckRun",
                checkSuite: workflow,
                conclusion: "FAILURE",
                name: "test",
                startedAt: "2026-08-01T10:00:00Z",
                status: "COMPLETED",
              },
              {
                __typename: "CheckRun",
                checkSuite: workflow,
                conclusion: "SUCCESS",
                name: "test",
                startedAt: "2026-08-01T10:05:00Z",
                status: "COMPLETED",
              },
            ],
          },
        ],
      }),
      "check_run",
      completedCheckRun,
    );
    await drainBackgroundWork(1_000);
    expect(emits[0]!.body.payload).toEqual({ failed: false, failing: [] });
  });

  test("fails closed on unreadable context detail while aggregate counts are pending", async () => {
    const emits: EmitCall[] = [];
    await handleCiEvent(
      emitCtx(emits, {
        rollupSequence: [
          {
            state: "FAILURE",
            contexts: [null],
            partial: true,
            checkRunCountsByState: [
              { count: 1, state: "FAILURE" },
              { count: 1, state: "IN_PROGRESS" },
            ],
            statusContextCountsByState: [],
          },
        ],
      }),
      "check_run",
      completedCheckRun,
    );
    expect(emits.length).toBe(0);
  });

  test("does not emit on a momentary green — the registration race", async () => {
    // Push lands, no-op checks complete first, the rollup reads SUCCESS for a
    // few seconds before the real suite registers. The confirm re-read must
    // catch it flipping PENDING and suppress the emission.
    const emits: EmitCall[] = [];
    await handleCiEvent(
      emitCtx(emits, { rollupSequence: [{ state: "SUCCESS" }, { state: "PENDING" }] }),
      "check_run",
      completedCheckRun,
    );
    await drainBackgroundWork(1_000);
    expect(emits.length).toBe(0);
  });

  test("does not emit ci-completed when the rollup is unreadable", async () => {
    // A failed read is UNKNOWN, not settled — emitting would manufacture a
    // green signal out of thin air (e.g. a token missing checks:read).
    const emits: EmitCall[] = [];
    await handleCiEvent(emitCtx(emits, { rollupSequence: [{ fail: true }] }), "check_run", completedCheckRun);
    expect(emits.length).toBe(0);
  });

  test("does not emit for an in-flight check run or a pending status", async () => {
    const emits: EmitCall[] = [];
    const ctx = emitCtx(emits);
    await handleCiEvent(
      ctx,
      "check_run",
      JSON.stringify({
        action: "created",
        repository: { full_name: "base/repo" },
        check_run: { head_sha: "abc123", pull_requests: [] },
      }),
    );
    await handleCiEvent(
      ctx,
      "status",
      JSON.stringify({
        repository: { full_name: "base/repo" },
        sha: "abc123",
        state: "pending",
      }),
    );
    expect(emits.length).toBe(0);
  });

  test("does not emit when workflowEvents is off", async () => {
    const emits: EmitCall[] = [];
    await handleCiEvent(
      emitCtx(emits, { workflowEvents: false }),
      "check_run",
      completedCheckRun,
    );
    expect(emits.length).toBe(0);
  });

  test("emits review-submitted keyed by PR, head sha, and reviewer", async () => {
    const emits: EmitCall[] = [];
    await handleReviewEvent(
      emitCtx(emits),
      JSON.stringify({
        action: "submitted",
        repository: { full_name: "base/repo" },
        pull_request: { number: 7 },
        review: {
          commit_id: "reviewed456",
          id: 123,
          state: "commented",
          user: { login: "chatgpt-codex-connector" },
        },
      }),
    );
    await drainBackgroundWork(1_000);
    expect(emits.length).toBe(1);
    expect(emits[0]!.body).toEqual({
      event_type: "review-submitted",
      correlation_id: "base/repo:pr-7:reviewed456:chatgpt-codex-connector",
      payload: { review_id: 123, state: "commented" },
    });
  });

  test("reviews from different authors get independent rows, never collapsing", async () => {
    const emits: EmitCall[] = [];
    const submittedReview = (id: number, login: string) =>
      JSON.stringify({
        action: "submitted",
        repository: { full_name: "base/repo" },
        pull_request: { number: 7 },
        review: { commit_id: "abc123", id, state: "commented", user: { login } },
      });
    const ctx = emitCtx(emits);
    await handleReviewEvent(ctx, submittedReview(125, "human-reviewer"));
    await handleReviewEvent(ctx, submittedReview(126, "chatgpt-codex-connector"));
    await drainBackgroundWork(1_000);
    expect(emits.map((e) => e.body.correlation_id)).toEqual([
      "base/repo:pr-7:abc123:human-reviewer",
      "base/repo:pr-7:abc123:chatgpt-codex-connector",
    ]);
  });

  test("does not delay PR management while a review event is being delivered", async () => {
    let finishDelivery!: (response: Response) => void;
    const delivery = new Promise<Response>((resolve) => {
      finishDelivery = resolve;
    });
    const ctx = emitCtx([]);
    ctx.options.fetch = () => delivery;

    await handleReviewEvent(
      ctx,
      JSON.stringify({
        action: "submitted",
        repository: { full_name: "base/repo" },
        pull_request: { number: 7 },
        review: {
          commit_id: "abc123",
          id: 127,
          state: "commented",
          user: { login: "human-reviewer" },
        },
      }),
    );

    finishDelivery(new Response("", { status: 200 }));
    await drainBackgroundWork(1_000);
  });

  test("paginates rollup contexts before deduplicating reruns", async () => {
    const ctx = emitCtx([]);
    ctx.octokit.graphql = (async (_query: string, variables: { after?: string }) => ({
      repository: {
        object: {
          statusCheckRollup: {
            state: "SUCCESS",
            contexts:
              variables.after === "page-2"
                ? {
                    nodes: [
                      {
                        __typename: "CheckRun",
                        conclusion: "SUCCESS",
                        name: "test",
                        startedAt: "2026-08-01T10:05:00Z",
                        status: "COMPLETED",
                      },
                    ],
                    pageInfo: { hasNextPage: false, endCursor: null },
                  }
                : {
                    nodes: [
                      {
                        __typename: "CheckRun",
                        conclusion: "FAILURE",
                        name: "test",
                        startedAt: "2026-08-01T10:00:00Z",
                        status: "COMPLETED",
                      },
                    ],
                    pageInfo: { hasNextPage: true, endCursor: "page-2" },
                  },
          },
        },
      },
    })) as typeof ctx.octokit.graphql;

    await expect(fetchCiEvaluation(ctx, "base", "repo", "abc123")).resolves.toEqual({
      failed: false,
      failingNames: [],
      settled: true,
    });
  });

  test("retries transient workflow event delivery", async () => {
    let attempts = 0;
    await emitWorkflowEvent(
      {
        apiUrl: "http://localhost",
        token: "test-token",
        webhookSecret: "test-secret",
        fetch: () => {
          attempts += 1;
          return Promise.resolve(
            new Response("", { status: attempts === 1 ? 503 : 200 }),
          );
        },
      },
      {
        correlationId: "base/repo:abc123",
        eventType: "ci-completed",
        payload: {},
      },
    );
    expect(attempts).toBe(2);
  });
});

describe("management turn reaction ack", () => {
  const submittedReview = (state: string, nodeId?: string) =>
    JSON.stringify({
      action: "submitted",
      repository: { full_name: "base/repo" },
      pull_request: { number: 7 },
      review: {
        id: 55,
        node_id: nodeId,
        state,
        user: { login: "reviewer" },
      },
    });

  function reviewCtx(reactions: { subjectId: string; content: string }[]) {
    return {
      octokit: {
        graphql: async (
          _query: string,
          vars: { subjectId: string; content: string },
        ) => {
          reactions.push({ subjectId: vars.subjectId, content: vars.content });
          return {};
        },
        rest: {
          pulls: {
            get: async () => ({
              data: prPayload({ headRepoFullName: "base/repo" }),
            }),
            merge: async () => ({ data: {} }),
          },
          git: { deleteRef: async () => ({ data: {} }) },
        },
      },
      options: {
        apiUrl: "http://localhost",
        deleteBranchOnMerge: false,
        logger: quietLogger,
        // Non-retryable so the backgrounded turn settles off the network.
        fetch: () => Promise.resolve(new Response("no", { status: 400 })),
      },
      state: makeState(),
      userName: "centaur-bot",
    } as unknown as PrManagerContext;
  }

  test("acks a changes-requested review with eyes on the review itself, settling when the turn fails", async () => {
    const reactions: { subjectId: string; content: string }[] = [];
    await handleReviewEvent(
      reviewCtx(reactions),
      submittedReview("changes_requested", "PRR_test55"),
    );
    // The working ack lands before the management turn runs.
    expect(reactions).toContainEqual({ subjectId: "PRR_test55", content: "EYES" });
    await drainBackgroundWork(5_000);
    expect(reactions).toContainEqual({
      subjectId: "PRR_test55",
      content: "CONFUSED",
    });
  });

  test("does not react on an approved review (deterministic merge, no work turn)", async () => {
    const reactions: { subjectId: string; content: string }[] = [];
    await handleReviewEvent(
      reviewCtx(reactions),
      submittedReview("approved", "PRR_test55"),
    );
    await drainBackgroundWork(5_000);
    expect(reactions).toEqual([]);
  });

  test("stays quiet when the payload carries no review node id", async () => {
    const reactions: { subjectId: string; content: string }[] = [];
    await handleReviewEvent(reviewCtx(reactions), submittedReview("changes_requested"));
    await drainBackgroundWork(5_000);
    expect(reactions).toEqual([]);
  });
});
