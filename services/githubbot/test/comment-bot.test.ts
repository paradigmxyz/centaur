import { describe, expect, test } from "bun:test";
import { buildTurnReplyBody } from "../src/comment-bot";

const turn = {
  answer: "Done.",
  cotLines: ["Bash: gh api repos/o/internal-repo/contents/notes.md"],
  errorText: "",
  failed: false,
  fallbackText: "",
};

describe("buildTurnReplyBody", () => {
  test("posts the chain of thought on a private repository", () => {
    const body = buildTurnReplyBody(turn, true);
    expect(body).toStartWith("Done.");
    expect(body).toContain("<summary>Chain of thought</summary>");
    expect(body).toContain("internal-repo");
  });

  test("posts only the answer otherwise", () => {
    expect(buildTurnReplyBody(turn, false)).toBe("Done.");
  });

  test("keeps the chain of thought off a failed turn's reply too", () => {
    const failed = { ...turn, answer: "", errorText: "sandbox lost", failed: true };
    expect(buildTurnReplyBody(failed, false)).toBe(
      "⚠️ I ran into an error before finishing:\n\nsandbox lost",
    );
    expect(buildTurnReplyBody(failed, true)).toContain("<details>");
  });
});
