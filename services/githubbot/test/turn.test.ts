import { describe, expect, test } from "bun:test";
import {
  githubContextPreamble,
  parseGithubThreadKey,
  reviewCommentContextFromRaw,
} from "../src/turn";

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
