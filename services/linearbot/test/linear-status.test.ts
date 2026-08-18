import { describe, expect, it } from "bun:test";
import {
  extractStatusMarker,
  fetchIssueStatus,
  kickoffTargetState,
  markerTargetState,
  pickWorkflowState,
  type LinearIssueStatus,
  type LinearWorkflowState,
} from "../src/linear-status";
import type { LinearRawRequestClient } from "../src/types";

const STATES: LinearWorkflowState[] = [
  { id: "st-triage", name: "Triage", position: 0, type: "triage" },
  { id: "st-todo", name: "Todo", position: 1, type: "unstarted" },
  { id: "st-review", name: "In Review", position: 3, type: "started" },
  { id: "st-progress", name: "In Progress", position: 2, type: "started" },
  { id: "st-done", name: "Done", position: 4, type: "completed" },
];

function status(input: Partial<LinearIssueStatus> = {}): LinearIssueStatus {
  return { states: STATES, ...input };
}

describe("extractStatusMarker", () => {
  it("extracts and strips a trailing marker line", () => {
    const { marker, text } = extractStatusMarker(
      "All done, see the PR.\n\nLinear-Status: done",
    );
    expect(marker).toBe("done");
    expect(text).toBe("All done, see the PR.");
  });

  it("accepts in-progress spellings and is case-insensitive", () => {
    expect(extractStatusMarker("x\nlinear-status: In Progress").marker).toBe(
      "in_progress",
    );
    expect(extractStatusMarker("x\nLINEAR-STATUS: in_progress").marker).toBe(
      "in_progress",
    );
    expect(extractStatusMarker("x\nLinear-Status: todo").marker).toBe("todo");
  });

  it("last marker wins and all marker lines are stripped", () => {
    const { marker, text } = extractStatusMarker(
      "Linear-Status: todo\nhalfway\nLinear-Status: done",
    );
    expect(marker).toBe("done");
    expect(text).toBe("halfway");
  });

  it("ignores markers embedded mid-line", () => {
    const input = "I would set Linear-Status: done if I could.";
    const { marker, text } = extractStatusMarker(input);
    expect(marker).toBeUndefined();
    expect(text).toBe(input);
  });
});

describe("pickWorkflowState", () => {
  it("returns the lowest-position state of the type", () => {
    expect(pickWorkflowState(STATES, "started")?.id).toBe("st-progress");
  });

  it("returns undefined when the team has no state of the type", () => {
    expect(pickWorkflowState(STATES, "canceled")).toBeUndefined();
  });
});

describe("kickoffTargetState", () => {
  it.each(["triage", "backlog", "unstarted"])(
    "moves a %s issue to the first started state",
    (stateType) => {
      expect(kickoffTargetState(status({ stateType }))?.id).toBe("st-progress");
    },
  );

  it.each(["started", "completed", "canceled", undefined])(
    "never demotes an issue whose state type is %s",
    (stateType) => {
      expect(kickoffTargetState(status({ stateType }))).toBeUndefined();
    },
  );
});

describe("markerTargetState", () => {
  it("maps done/in_progress/todo to the matching state types", () => {
    expect(
      markerTargetState(status({ stateType: "started" }), "done")?.id,
    ).toBe("st-done");
    expect(
      markerTargetState(status({ stateType: "completed" }), "in_progress")?.id,
    ).toBe("st-progress");
    expect(
      markerTargetState(status({ stateType: "started" }), "todo")?.id,
    ).toBe("st-todo");
  });

  it("declines when the issue is already in a state of the target type", () => {
    expect(
      markerTargetState(status({ stateType: "completed" }), "done"),
    ).toBeUndefined();
  });

  it("does not mark review states as done", () => {
    expect(
      markerTargetState(
        status({ stateName: "In Review", stateType: "started" }),
        "done",
      ),
    ).toBeUndefined();
  });

  it("still allows review states to move back to todo or in progress", () => {
    expect(
      markerTargetState(
        status({ stateName: "In Review", stateType: "started" }),
        "todo",
      )?.id,
    ).toBe("st-todo");
    expect(
      markerTargetState(
        status({ stateName: "In Review", stateType: "unstarted" }),
        "in_progress",
      )?.id,
    ).toBe("st-progress");
  });
});

function stubClient(data: unknown): LinearRawRequestClient {
  return {
    client: { rawRequest: async <Data>() => ({ data: data as Data }) },
  };
}

describe("fetchIssueStatus", () => {
  it("parses delegate, state, and team states from the raw response", async () => {
    const result = await fetchIssueStatus(
      stubClient({
        issue: {
          delegate: { id: "bot-user-1" },
          state: { id: "st-todo", name: "Todo", type: "unstarted" },
          team: {
            states: {
              nodes: [
                {
                  id: "st-progress",
                  name: "In Progress",
                  position: 2,
                  type: "started",
                },
                { id: "bogus" },
              ],
            },
          },
        },
      }),
      "issue-1",
    );
    expect(result).toEqual({
      delegateId: "bot-user-1",
      stateId: "st-todo",
      stateName: "Todo",
      stateType: "unstarted",
      states: [
        {
          id: "st-progress",
          name: "In Progress",
          position: 2,
          type: "started",
        },
      ],
    });
  });

  it("returns null when the issue is missing or the client is unusable", async () => {
    expect(
      await fetchIssueStatus(stubClient({ issue: null }), "issue-1"),
    ).toBeNull();
    expect(await fetchIssueStatus({}, "issue-1")).toBeNull();
  });
});
