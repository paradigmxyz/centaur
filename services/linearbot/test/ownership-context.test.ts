import { describe, expect, it } from "bun:test";

import {
  DEFAULT_EMPTY_PROMPT_INSTRUCTION,
  DEFAULT_OWNERSHIP_CONTEXT,
  emptyPromptInstruction,
  ownershipContext,
} from "../src/linear-context";

/** The shape a deployment that gates Done behind a human actually needs. */
const HUMAN_VERIFIED_CONTRACT = [
  "You own this Linear issue. Carry the work forward and complete it if you can.",
  '- Never move the issue to "Done": merging lands it in "UAT" for a human to verify.',
  "- If you could not make progress, end your final answer with `Linear-Status: todo`.",
].join("\n");

describe("ownershipContext", () => {
  it("uses the default contract when none is configured", () => {
    for (const configured of [undefined, "", "   "]) {
      expect(ownershipContext(configured)).toBe(DEFAULT_OWNERSHIP_CONTEXT);
    }
  });

  it("replaces the whole contract rather than extending it", () => {
    const context = ownershipContext(HUMAN_VERIFIED_CONTRACT);

    expect(context).toBe(HUMAN_VERIFIED_CONTRACT);
    // Appending would leave both policies in context, which is the exact
    // contradiction the override exists to remove.
    expect(context).not.toContain('move it to "Done" if the work is complete');
    expect(context).not.toContain("Linear-Status: done");
  });

  it("lets a deployment narrow the marker backstop, not just the prose", () => {
    // The terminal-state policy is not confined to one clause: a deployment
    // that gates Done behind a human must also stop the agent setting `done`
    // through the marker. A clause-level override could not express this.
    const context = ownershipContext(HUMAN_VERIFIED_CONTRACT);

    expect(context).toContain("Linear-Status: todo");
    expect(context).not.toContain("Linear-Status: done");
  });

  it("trims a configured contract", () => {
    expect(ownershipContext(`  ${HUMAN_VERIFIED_CONTRACT}  `)).toBe(
      HUMAN_VERIFIED_CONTRACT,
    );
  });
});

describe("emptyPromptInstruction", () => {
  it("falls back to the default when unset or blank", () => {
    for (const configured of [undefined, "", "   "]) {
      expect(emptyPromptInstruction(configured)).toBe(
        DEFAULT_EMPTY_PROMPT_INSTRUCTION,
      );
    }
  });

  it("uses the configured instruction when set", () => {
    expect(emptyPromptInstruction("Work the issue and open a PR.")).toBe(
      "Work the issue and open a PR.",
    );
  });
});
