import { describe, expect, test } from "bun:test";
import {
  githubContextPreamble,
  parseGithubThreadKey,
  reviewCommentContextFromRaw,
  runSessionTurn,
  turnOutputChars,
  type TurnResult,
} from "../src/turn";
import { FAKE_TURN_COMMAND, fakeSessionApiFetch } from "./fake-session-api";

describe("parseGithubThreadKey", () => {
  test("parses a PR-level thread key", () => {
    expect(parseGithubThreadKey("github:0xSplits/centaur:123")).toEqual({
      owner: "0xSplits",
      repo: "centaur",
      number: 123,
      type: "pr",
    });
  });

  test("parses an issue thread key", () => {
    expect(parseGithubThreadKey("github:0xSplits/centaur:issue:42")).toEqual({
      owner: "0xSplits",
      repo: "centaur",
      number: 42,
      type: "issue",
    });
  });

  test("parses a review-comment thread key", () => {
    expect(
      parseGithubThreadKey("github:0xSplits/centaur:123:rc:99887766"),
    ).toEqual({
      owner: "0xSplits",
      repo: "centaur",
      number: 123,
      type: "pr",
      reviewCommentId: 99887766,
    });
  });

  test("returns null for non-github / malformed / synthetic keys", () => {
    expect(parseGithubThreadKey("linear:abc:c:def")).toBeNull();
    expect(parseGithubThreadKey("github:no-repo:1")).toBeNull();
    expect(parseGithubThreadKey("github:owner/repo:notanumber")).toBeNull();
    // The isolated review/issue-work/management thread keys are intentionally
    // not postable github keys.
    expect(parseGithubThreadKey("github-review:0xSplits/centaur:7")).toBeNull();
    expect(parseGithubThreadKey("github-issue:0xSplits/centaur:7")).toBeNull();
    expect(parseGithubThreadKey("github-manage:0xSplits/centaur:7")).toBeNull();
  });
});

describe("reviewCommentContextFromRaw", () => {
  test("extracts path/line/hunk from a review_comment raw message", () => {
    expect(
      reviewCommentContextFromRaw({
        type: "review_comment",
        comment: {
          path: "src/index.ts",
          line: 42,
          diff_hunk: "@@ -1 +1 @@\n-old\n+new",
        },
      }),
    ).toEqual({
      path: "src/index.ts",
      line: 42,
      diffHunk: "@@ -1 +1 @@\n-old\n+new",
    });
  });

  test("returns undefined for non-review-comment messages", () => {
    expect(reviewCommentContextFromRaw({ type: "issue_comment" })).toBeUndefined();
    expect(reviewCommentContextFromRaw(null)).toBeUndefined();
    expect(reviewCommentContextFromRaw("nope")).toBeUndefined();
  });
});

describe("githubContextPreamble", () => {
  test("PR conversation: names the main thread and tells it to fetch the PR", () => {
    const preamble = githubContextPreamble("github:0xSplits/centaur:123");
    expect(preamble).toContain("main conversation thread");
    expect(preamble).toContain("0xSplits/centaur#123");
    expect(preamble).toContain("gh pr diff 123");
  });

  test("issue thread: uses issue wording", () => {
    const preamble = githubContextPreamble("github:0xSplits/centaur:issue:42");
    expect(preamble).toContain("issue 0xSplits/centaur#42");
    expect(preamble).toContain("gh issue view 42");
  });

  test("review-comment thread: anchors to the file/line and includes the hunk", () => {
    const preamble = githubContextPreamble(
      "github:0xSplits/centaur:123:rc:55",
      { path: "src/turn.ts", line: 88, diffHunk: "@@ -1 +1 @@\n+x" },
    );
    expect(preamble).toContain("review-comment thread");
    expect(preamble).toContain("`src/turn.ts` line 88");
    expect(preamble).toContain("@@ -1 +1 @@");
  });

  test("returns undefined for an unparseable key", () => {
    expect(githubContextPreamble("not-a-github-key")).toBeUndefined();
  });
});

describe("turnOutputChars", () => {
  function result(overrides: Partial<TurnResult> = {}): TurnResult {
    return {
      answer: "",
      cotLines: [],
      errorText: "",
      failed: false,
      fallbackText: "",
      ...overrides,
    };
  }

  test("measures the answer a turn produced", () => {
    expect(turnOutputChars(result({ answer: "done" }))).toBe(4);
  });

  test("falls back to the fallback text when there is no answer", () => {
    expect(turnOutputChars(result({ fallbackText: "twelve chars" }))).toBe(12);
  });

  test("reports the error text for a failed turn", () => {
    expect(
      turnOutputChars(result({ errorText: "boom", failed: true })),
    ).toBe(4);
  });

  test("is zero only when a successful turn really produced nothing", () => {
    expect(turnOutputChars(result())).toBe(0);
  });
});

describe("runSessionTurn", () => {
  type TurnInput = Parameters<typeof runSessionTurn>[0];

  async function replyFor(get: () => Promise<unknown>): Promise<string[]> {
    const posted: string[] = [];
    const repoLookups: unknown[] = [];
    let state = {};
    await runSessionTurn({
      adapter: {
        octokit: {
          rest: {
            repos: {
              get: (params: unknown) => {
                repoLookups.push(params);
                return get();
              },
            },
          },
        },
      } as unknown as TurnInput["adapter"],
      executeMessage: {
        attachments: [],
        author: {
          fullName: "Someone",
          isBot: false,
          isMe: false,
          userId: "1",
          userName: "someone",
        },
        id: "c1",
        isMention: true,
        raw: {},
        text: "@centaur-bot summarize the notes",
        threadId: "github:o/r:7",
        timestamp: "2026-01-01T00:00:00Z",
      },
      options: {
        apiUrl: "http://api.test",
        fetch: fakeSessionApiFetch("Done."),
        token: "test-token",
        webhookSecret: "test-secret",
      },
      overrides: {},
      thread: {
        get state() {
          return Promise.resolve(state);
        },
        setState: async (next: object) => {
          state = { ...state, ...next };
        },
        post: async (body: string) => {
          posted.push(body);
        },
      } as unknown as TurnInput["thread"],
      threadKey: "github:o/r:7",
      trace: {
        includeContext: false,
        messageId: "c1",
        mode: "execute",
        openStream: true,
        startedAtMs: 0,
        threadId: "github:o/r:7",
      },
    });
    expect(repoLookups).toEqual([{ owner: "o", repo: "r" }]);
    return posted;
  }

  test("posts the chain of thought on a private repository", async () => {
    const [reply, ...rest] = await replyFor(async () => ({ data: { private: true } }));
    expect(rest).toEqual([]);
    expect(reply).toStartWith("Done.");
    expect(reply).toContain(FAKE_TURN_COMMAND);
  });

  test("posts the answer alone on a public repository", async () => {
    expect(await replyFor(async () => ({ data: { private: false } }))).toEqual(["Done."]);
  });

  test("posts the answer alone when the repository can't be read", async () => {
    const posted = await replyFor(async () => {
      throw Object.assign(new Error("Not Found"), { status: 404 });
    });
    expect(posted).toEqual(["Done."]);
  });
});
