import { describe, expect, test } from "bun:test";
import {
  agentSessionIdFromThreadKey,
  parseLinearThreadKey,
} from "../src/linear-threading";

describe("parseLinearThreadKey", () => {
  test("parses issue-level keys", () => {
    expect(parseLinearThreadKey("linear:issue-1")).toEqual({
      issueId: "issue-1",
    });
  });

  test("parses comment-thread keys", () => {
    expect(parseLinearThreadKey("linear:issue-1:c:comment-2")).toEqual({
      issueId: "issue-1",
      commentId: "comment-2",
    });
  });

  test("parses agent-session keys", () => {
    expect(parseLinearThreadKey("linear:issue-1:s:sess-3")).toEqual({
      issueId: "issue-1",
      agentSessionId: "sess-3",
    });
  });

  test("parses comment-scoped agent-session keys", () => {
    expect(parseLinearThreadKey("linear:issue-1:c:comment-2:s:sess-3")).toEqual(
      {
        issueId: "issue-1",
        commentId: "comment-2",
        agentSessionId: "sess-3",
      },
    );
  });

  test("returns empty for non-linear keys", () => {
    expect(parseLinearThreadKey("slack:C1:170.001")).toEqual({});
    expect(parseLinearThreadKey("linear:")).toEqual({});
  });
});

describe("agentSessionIdFromThreadKey", () => {
  test("extracts the session id when present", () => {
    expect(agentSessionIdFromThreadKey("linear:i:s:sess-9")).toBe("sess-9");
    expect(agentSessionIdFromThreadKey("linear:i:c:c1")).toBeUndefined();
    expect(agentSessionIdFromThreadKey("linear:i")).toBeUndefined();
  });
});
