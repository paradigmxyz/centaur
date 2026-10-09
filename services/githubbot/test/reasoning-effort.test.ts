import { describe, expect, it } from "bun:test";

import {
  normalizeReasoningEffort,
  reasoningEffortFor,
} from "../src/reasoning-effort";

describe("normalizeReasoningEffort", () => {
  it("accepts every codex effort", () => {
    for (const effort of [
      "none",
      "minimal",
      "low",
      "medium",
      "high",
      "xhigh",
      "max",
      "ultra",
    ]) {
      expect(normalizeReasoningEffort(effort)).toBe(effort);
    }
  });

  it("accepts the same aliases slackbotv2's -rsn flag does", () => {
    // Kept in step deliberately: a value that works in a -rsn flag should work
    // in this config, or the two surfaces disagree about the same word.
    expect(normalizeReasoningEffort("min")).toBe("minimal");
    expect(normalizeReasoningEffort("med")).toBe("medium");
    expect(normalizeReasoningEffort("hi")).toBe("high");
    expect(normalizeReasoningEffort("xhi")).toBe("xhigh");
    expect(normalizeReasoningEffort("x-high")).toBe("xhigh");
  });

  it("is case and whitespace insensitive", () => {
    expect(normalizeReasoningEffort("  XHigh ")).toBe("xhigh");
  });

  it("drops an unrecognised value rather than forwarding it", () => {
    // Forwarding a typo verbatim reaches the harness as an invalid
    // turn/start.effort and fails the turn. Running at the default is the
    // better failure, especially for config set once at deploy time.
    for (const value of [undefined, "", "   ", "extreme", "very-high", "9"]) {
      expect(normalizeReasoningEffort(value)).toBeUndefined();
    }
  });
});

describe("reasoningEffortFor", () => {
  it("resolves each turn type independently", () => {
    const policy = {
      assignment: "xhigh",
      "ci-fix": "low",
      comment: "medium",
      management: "medium",
      review: "high",
    };
    expect(reasoningEffortFor(policy, "assignment")).toBe("xhigh");
    expect(reasoningEffortFor(policy, "ci-fix")).toBe("low");
    expect(reasoningEffortFor(policy, "comment")).toBe("medium");
    expect(reasoningEffortFor(policy, "management")).toBe("medium");
    expect(reasoningEffortFor(policy, "review")).toBe("high");
  });

  it("leaves a turn type unset when only others are configured", () => {
    // Setting one must not imply the others: a deployment that wants deep
    // issue-work turns has not thereby asked for deep CI fixes.
    const policy = { assignment: "high" };
    expect(reasoningEffortFor(policy, "assignment")).toBe("high");
    expect(reasoningEffortFor(policy, "ci-fix")).toBeUndefined();
    expect(reasoningEffortFor(policy, "comment")).toBeUndefined();
    expect(reasoningEffortFor(policy, "management")).toBeUndefined();
    expect(reasoningEffortFor(policy, "review")).toBeUndefined();
  });

  it("falls back to the harness default when unconfigured", () => {
    expect(reasoningEffortFor(undefined, "assignment")).toBeUndefined();
    expect(reasoningEffortFor({}, "review")).toBeUndefined();
  });
});
