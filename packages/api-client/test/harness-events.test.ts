import { describe, expect, it } from "vitest";

import { normalizeHarnessEvent } from "@centaur/harness-events";

describe("normalizeHarnessEvent", () => {
  it("normalizes OpenRouter thread starts through the Codex event path", () => {
    expect(
      normalizeHarnessEvent("openrouter", {
        type: "thread.started",
        thread_id: "thread-or",
      }),
    ).toEqual([{ type: "system", subtype: "init", session_id: "thread-or" }]);
  });

  it("normalizes OpenRouter turn completion usage through the Codex event path", () => {
    expect(
      normalizeHarnessEvent("openrouter", {
        type: "turn.completed",
        model: "openai/gpt-4o-mini",
        usage: { input_tokens: 3, output_tokens: 5 },
      }),
    ).toEqual([
      {
        type: "usage",
        usage: { input_tokens: 3, output_tokens: 5 },
        model: "openai/gpt-4o-mini",
        authoritative: true,
      },
    ]);
  });

  it("passes OpenRouter Codex item events through instead of treating them as amp events", () => {
    const event = {
      type: "item.completed",
      item: { type: "agent_message", text: "hello" },
    };

    expect(normalizeHarnessEvent("openrouter", event)).toEqual([event]);
  });
});
