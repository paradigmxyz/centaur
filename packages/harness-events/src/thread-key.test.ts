import { describe, expect, it } from "vitest";

import { normalizeThreadKey, splitThreadKey } from "./thread-key";

describe("splitThreadKey", () => {
  it("parses two-part channel:ts keys", () => {
    expect(splitThreadKey("C123:1700000000.000100")).toEqual({
      channel: "C123",
      threadTs: "1700000000.000100",
    });
  });

  it("parses three-part slack:channel:ts keys", () => {
    expect(splitThreadKey("slack:C123:1700000000.000100")).toEqual({
      channel: "C123",
      threadTs: "1700000000.000100",
    });
  });

  it("parses four-part team-qualified Slack keys", () => {
    expect(splitThreadKey("slack:T1:C123:1700000000.000100")).toEqual({
      channel: "C123",
      threadTs: "1700000000.000100",
    });
  });

  it("rejects malformed keys", () => {
    expect(() => splitThreadKey("incomplete")).toThrow(/Invalid thread key format/);
    expect(() => splitThreadKey("slack:team:channel:ts:extra")).toThrow(
      /Invalid thread key format/
    );
    expect(() => splitThreadKey("")).toThrow(/Invalid thread key format/);
  });
});

describe("normalizeThreadKey", () => {
  it("normalizes all supported Slack shapes to channel:ts", () => {
    expect(normalizeThreadKey("C123:1700000000.000100")).toBe("C123:1700000000.000100");
    expect(normalizeThreadKey("slack:C123:1700000000.000100")).toBe("C123:1700000000.000100");
    expect(normalizeThreadKey("slack:T1:C123:1700000000.000100")).toBe(
      "C123:1700000000.000100"
    );
  });
});
