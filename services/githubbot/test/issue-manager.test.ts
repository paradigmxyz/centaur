import { describe, expect, test } from "bun:test";
import {
  assigneeLogins,
  isAssignedToBot,
  issueWorkThreadKey,
} from "../src/issue-manager";

describe("isAssignedToBot", () => {
  test("true when the bot is among the assignees (case-insensitive)", () => {
    expect(isAssignedToBot(["someone", "Centaur-Bot"], "centaur-bot")).toBe(
      true,
    );
  });

  test("false when the bot is not an assignee", () => {
    expect(isAssignedToBot(["someone"], "centaur-bot")).toBe(false);
  });

  test("false with no assignees", () => {
    expect(isAssignedToBot([], "centaur-bot")).toBe(false);
  });
});

describe("assigneeLogins", () => {
  test("extracts logins, skipping malformed entries", () => {
    expect(
      assigneeLogins([
        { login: "alice" },
        null,
        {},
        { login: "" },
        { login: "bob" },
      ]),
    ).toEqual(["alice", "bob"]);
  });

  test("returns [] for non-array input", () => {
    expect(assigneeLogins(undefined)).toEqual([]);
    expect(assigneeLogins(null)).toEqual([]);
    expect(assigneeLogins("nope")).toEqual([]);
  });
});

describe("issueWorkThreadKey", () => {
  test("builds the isolated work-session key", () => {
    expect(issueWorkThreadKey("0xSplits", "centaur", 7)).toBe(
      "github-issue:0xSplits/centaur:7",
    );
  });
});
