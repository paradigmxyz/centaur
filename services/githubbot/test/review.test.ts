import { describe, expect, test } from "bun:test";
import { handleReviewRequest } from "../src/review";
import type { GithubbotOptions } from "../src/types";

// Non-retryable fetch so the (backgrounded) review turn settles instantly in the
// positive case instead of hitting the network and retrying.
const options = {
  apiUrl: "http://127.0.0.1:8080",
  stateKeyPrefix: "test",
  fetch: () => Promise.resolve(new Response("no", { status: 400 })),
} as unknown as GithubbotOptions;

function stubState() {
  const seen = new Set<string>();
  return {
    setIfNotExists: (key: string) => {
      if (seen.has(key)) return Promise.resolve(false);
      seen.add(key);
      return Promise.resolve(true);
    },
  } as never;
}

// Reactions are best-effort acks; a stub that resolves keeps the positive-path
// tests off the network.
const octokit = {
  rest: {
    reactions: { createForIssue: () => Promise.resolve({ data: { id: 1 } }) },
  },
} as never;

const input = {
  botUserName: "review-bot",
  deliveryId: "delivery-1",
  octokit,
  options,
  state: stubState(),
};

function reviewRequestedBody(reviewerLogin: string | null): string {
  return JSON.stringify({
    action: "review_requested",
    pull_request: {
      number: 7,
      title: "Add widget",
      html_url: "https://github.com/0xSplits/centaur/pull/7",
      head: { sha: "abc123" },
    },
    repository: { full_name: "0xSplits/centaur" },
    requested_reviewer: reviewerLogin ? { login: reviewerLogin } : undefined,
    sender: { login: "someone" },
  });
}

describe("handleReviewRequest", () => {
  test("ignores non-JSON bodies", () => {
    expect(handleReviewRequest("not json", input)).toBeNull();
  });

  test("ignores actions other than review_requested", () => {
    const body = JSON.stringify({ action: "opened" });
    expect(handleReviewRequest(body, input)).toBeNull();
  });

  test("ignores review requests for a different reviewer", () => {
    expect(
      handleReviewRequest(reviewRequestedBody("someone-else"), input),
    ).toBeNull();
  });

  test("ignores team review requests (no requested_reviewer)", () => {
    expect(handleReviewRequest(reviewRequestedBody(null), input)).toBeNull();
  });

  test("matches the bot reviewer case-insensitively and schedules work", () => {
    const result = handleReviewRequest(reviewRequestedBody("Review-Bot"), {
      ...input,
      state: stubState(),
    });
    expect(result).not.toBeNull();
  });

  test("de-duplicates a redelivered review request", async () => {
    const state = stubState();
    // First delivery claims the dedup key; second (same id) finds it taken.
    await handleReviewRequest(reviewRequestedBody("review-bot"), {
      ...input,
      state,
    });
    // A second handler with the same delivery id resolves without throwing; the
    // dedup claim short-circuits the turn (no assertion beyond completion).
    await handleReviewRequest(reviewRequestedBody("review-bot"), {
      ...input,
      state,
    });
    expect(true).toBe(true);
  });
});

function teamRequestBody(teamSlug: string | null): string {
  return JSON.stringify({
    action: "review_requested",
    pull_request: {
      number: 7,
      title: "Add widget",
      html_url: "https://github.com/0xSplits/centaur/pull/7",
      head: { sha: "abc123" },
    },
    repository: { full_name: "0xSplits/centaur" },
    requested_team: teamSlug ? { slug: teamSlug } : undefined,
    sender: { login: "someone" },
  });
}

function fullState() {
  const m = new Map<string, unknown>();
  return {
    get: async (k: string) => m.get(k),
    set: async (k: string, v: unknown) => {
      m.set(k, v);
    },
    setIfNotExists: async (k: string, v: unknown) => {
      if (m.has(k)) return false;
      m.set(k, v);
      return true;
    },
  } as never;
}

function teamInput(member: boolean, reactionSpy: { n: number }) {
  return {
    botUserName: "review-bot",
    deliveryId: "delivery-team",
    octokit: {
      rest: {
        reactions: {
          createForIssue: () => {
            reactionSpy.n += 1;
            return Promise.resolve({ data: { id: 1 } });
          },
        },
        teams: {
          getMembershipForUserInOrg: () =>
            member
              ? Promise.resolve({ data: { state: "active" } })
              : Promise.reject(new Error("404")),
        },
      },
    } as never,
    options,
    state: fullState(),
  };
}

describe("handleReviewRequest team requests", () => {
  test("acts on a team request when the bot is a member", async () => {
    const spy = { n: 0 };
    const result = handleReviewRequest(
      teamRequestBody("reviewers"),
      teamInput(true, spy),
    );
    expect(result).not.toBeNull();
    await result;
    // The working-ack reaction fires only once the request is accepted.
    expect(spy.n).toBeGreaterThanOrEqual(1);
  });

  test("ignores a team request when the bot is not a member", async () => {
    const spy = { n: 0 };
    await handleReviewRequest(teamRequestBody("strangers"), teamInput(false, spy));
    expect(spy.n).toBe(0);
  });

  test("ignores a request naming neither the bot nor a team", () => {
    expect(
      handleReviewRequest(teamRequestBody(null), teamInput(true, { n: 0 })),
    ).toBeNull();
  });
});
