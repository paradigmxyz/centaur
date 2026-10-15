import { describe, expect, it } from "vitest";
import { TurnCompletionFilter } from "./turn-filter";

const parentStarted = {
  method: "turn/started",
  params: { threadId: "thread-parent", turn: { id: "turn-parent" } },
};
const childStarted = {
  method: "turn/started",
  params: { threadId: "thread-child", turn: { id: "turn-child" } },
};
const childCompleted = {
  method: "turn/completed",
  params: {
    threadId: "thread-child",
    turn: { id: "turn-child", status: "completed" },
  },
};
const parentCompleted = {
  method: "turn/completed",
  params: {
    threadId: "thread-parent",
    turn: { id: "turn-parent", status: "completed" },
  },
};

describe("TurnCompletionFilter", () => {
  it("ignores collab child turn completions but accepts the root turn", () => {
    const filter = new TurnCompletionFilter();
    filter.noteLine(parentStarted);
    filter.noteLine(childStarted);
    expect(filter.isChildTurnCompletion(childCompleted)).toBe(true);
    expect(filter.isChildTurnCompletion(parentCompleted)).toBe(false);
  });

  it("treats completions as terminal when no root turn was seen", () => {
    const filter = new TurnCompletionFilter();
    expect(filter.isChildTurnCompletion(childCompleted)).toBe(false);
    expect(filter.isChildTurnCompletion(parentCompleted)).toBe(false);
  });

  it("treats id-less completions as terminal", () => {
    const filter = new TurnCompletionFilter();
    filter.noteLine(parentStarted);
    expect(filter.isChildTurnCompletion({ type: "turn.completed" })).toBe(
      false,
    );
  });

  it("pins the root on the first turn started even when it is id-less", () => {
    const filter = new TurnCompletionFilter();
    filter.noteLine({ method: "turn/started", params: {} });
    filter.noteLine(childStarted);
    expect(filter.isChildTurnCompletion(childCompleted)).toBe(false);
    expect(filter.isChildTurnCompletion(parentCompleted)).toBe(false);
  });

  it("keeps unseen parent completions terminal when resuming mid-turn", () => {
    const filter = new TurnCompletionFilter();
    filter.noteLine(childStarted);
    expect(filter.isChildTurnCompletion(parentCompleted)).toBe(false);
  });

  it("treats completions for never-started turns as terminal", () => {
    const filter = new TurnCompletionFilter();
    filter.noteLine(parentStarted);
    expect(filter.isChildTurnCompletion(childCompleted)).toBe(false);
  });

  it("flags child agent lines so their text stays out of the parent answer", () => {
    const filter = new TurnCompletionFilter();
    filter.noteLine(parentStarted);
    filter.noteLine(childStarted);
    expect(
      filter.isChildTurnLine({
        method: "item/agentMessage/delta",
        params: {
          threadId: "thread-child",
          turnId: "turn-child",
          delta: "child draft",
        },
      }),
    ).toBe(true);
    expect(
      filter.isChildTurnLine({
        method: "item/agentMessage/delta",
        params: {
          threadId: "thread-parent",
          turnId: "turn-parent",
          delta: "parent answer",
        },
      }),
    ).toBe(false);
  });
});
