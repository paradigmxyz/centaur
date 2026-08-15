import { describe, expect, test } from "bun:test";
import { extractMessageOverrides } from "../src/overrides";

describe("extractMessageOverrides", () => {
  test("returns text untouched without flags", () => {
    const result = extractMessageOverrides(
      "review this PR --not-a-known-flag stays",
    );
    expect(result).toEqual({
      cleanedText: "review this PR --not-a-known-flag stays",
      harnessType: undefined,
      model: undefined,
    });
  });

  test("parses harness flags", () => {
    expect(extractMessageOverrides("--claude review this")).toEqual({
      cleanedText: "review this",
      harnessType: "claudecode",
      model: undefined,
    });
    expect(
      extractMessageOverrides("--claude-code review this").harnessType,
    ).toBe("claudecode");
    expect(extractMessageOverrides("--amp review this").harnessType).toBe(
      "amp",
    );
    expect(extractMessageOverrides("--codex review this").harnessType).toBe(
      "codex",
    );
    expect(extractMessageOverrides("--nanocodex review this").harnessType).toBe(
      "nanocodex",
    );
  });

  test("parses harness flag anywhere in the message", () => {
    expect(extractMessageOverrides("review this --amp please")).toEqual({
      cleanedText: "review this please",
      harnessType: "amp",
      model: undefined,
    });
  });

  test("is case-insensitive", () => {
    expect(extractMessageOverrides("--Claude review").harnessType).toBe(
      "claudecode",
    );
  });

  test("parses --model with space or equals", () => {
    expect(
      extractMessageOverrides("--claude --model claude-sonnet-4-6 fix it"),
    ).toEqual({
      cleanedText: "fix it",
      harnessType: "claudecode",
      model: "claude-sonnet-4-6",
    });
    expect(extractMessageOverrides("--codex --model=gpt-5.2 fix it")).toEqual({
      cleanedText: "fix it",
      harnessType: "codex",
      model: "gpt-5.2",
    });
  });

  test("model shortcuts set model and imply claude-code", () => {
    expect(extractMessageOverrides("--opus fix it")).toEqual({
      cleanedText: "fix it",
      harnessType: "claudecode",
      model: "claude-opus-4-8",
    });
    expect(extractMessageOverrides("--sonnet fix it").model).toBe(
      "claude-sonnet-4-6",
    );
    expect(extractMessageOverrides("--haiku fix it").model).toBe(
      "claude-haiku-4-5",
    );
    expect(extractMessageOverrides("--fable fix it").model).toBe(
      "claude-fable-5",
    );
  });

  test("--meta selects the Meta provider and codex harness", () => {
    expect(extractMessageOverrides("--meta fix it")).toEqual({
      cleanedText: "fix it",
      harnessType: "codex",
      model: undefined,
      provider: "responses",
    });
  });

  test("--model expands claude aliases to full model ids", () => {
    expect(extractMessageOverrides("--claude --model opus go")).toEqual({
      cleanedText: "go",
      harnessType: "claudecode",
      model: "claude-opus-4-8",
    });
    expect(extractMessageOverrides("--model Sonnet go").model).toBe(
      "claude-sonnet-4-6",
    );
    expect(extractMessageOverrides("--model fable go").model).toBe(
      "claude-fable-5",
    );
  });

  test("--model accepts a newline immediately after the value", () => {
    expect(
      extractMessageOverrides("--claude --model=fable\nwhat model are you"),
    ).toEqual({
      cleanedText: "what model are you",
      harnessType: "claudecode",
      model: "claude-fable-5",
    });
    expect(
      extractMessageOverrides("@Centaur AI --claude --model=fable\r\nwhat model are you"),
    ).toEqual({
      cleanedText: "@Centaur AI what model are you",
      harnessType: "claudecode",
      model: "claude-fable-5",
    });
  });

  test("--model accepts a rendered line break immediately after the value", () => {
    expect(
      extractMessageOverrides("--claude --model=fable<br>what model are you"),
    ).toEqual({
      cleanedText: "what model are you",
      harnessType: "claudecode",
      model: "claude-fable-5",
    });
  });

  test("--model passes non-alias values through verbatim", () => {
    expect(
      extractMessageOverrides("--codex --model gpt-5.2-codex go").model,
    ).toBe("gpt-5.2-codex");
    expect(extractMessageOverrides("--amp --model fast go").model).toBe("fast");
  });

  test("explicit flags win over shortcut implications", () => {
    expect(extractMessageOverrides("--codex --opus fix it")).toEqual({
      cleanedText: "fix it",
      harnessType: "codex",
      model: "claude-opus-4-8",
    });
    expect(
      extractMessageOverrides("--sonnet --model claude-opus-4-8 fix it").model,
    ).toBe("claude-opus-4-8");
  });

  test("does not match flags embedded in words or longer flags", () => {
    expect(
      extractMessageOverrides("run pre--claude task").harnessType,
    ).toBeUndefined();
    expect(
      extractMessageOverrides("--claudette hi").harnessType,
    ).toBeUndefined();
    expect(extractMessageOverrides("--ampere hi").harnessType).toBeUndefined();
  });

  test("flag-only message cleans to empty text", () => {
    expect(extractMessageOverrides("--claude")).toEqual({
      cleanedText: "",
      harnessType: "claudecode",
      model: undefined,
    });
  });

  test("--model without a value is left untouched", () => {
    expect(extractMessageOverrides("what does --model do?")).toEqual({
      cleanedText: "what does --model do?",
      harnessType: undefined,
      model: undefined,
    });
    expect(extractMessageOverrides("--model\nwhat model are you")).toEqual({
      cleanedText: "--model\nwhat model are you",
      harnessType: undefined,
      model: undefined,
    });
  });
});
