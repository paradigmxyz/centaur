import { describe, expect, it } from "vitest";

import { displayLabel, formatAbsolute, formatCompact, formatRelative, pluralize, truncateMiddle } from "@/lib/format";

describe("truncateMiddle", () => {
  it("leaves short values unchanged", () => {
    expect(truncateMiddle("short")).toBe("short");
    expect(truncateMiddle(null)).toBe("");
  });

  it("keeps the head and tail around a center ellipsis", () => {
    const out = truncateMiddle("salesforce-marketing-cloud-rest-api", 24);
    expect(out).toHaveLength(24);
    expect(out.startsWith("salesforce")).toBe(true);
    expect(out.endsWith("rest-api")).toBe(true);
    expect(out).toContain("…");
  });

  it("respects a custom max and omission", () => {
    expect(truncateMiddle("abcdefghijklmnopqrstuvwxyz", 9, "...")).toBe("abc...xyz");
  });
});

describe("time formatting", () => {
  const now = new Date(2026, 5, 4, 18, 30, 0).getTime();

  it("formats absolute local time zero-padded", () => {
    expect(formatAbsolute(new Date(2026, 0, 2, 3, 4, 5))).toBe("01/02/2026 03:04:05");
  });

  it("formats compact ages", () => {
    expect(formatCompact(new Date(now - 30_000), now)).toBe("now");
    expect(formatCompact(new Date(now - 90_000), now)).toBe("1m");
    expect(formatCompact(new Date(now - 4 * 86_400_000), now)).toBe("4d");
    expect(formatCompact(new Date(now - 40 * 86_400_000), now)).toBe("1mo");
  });

  it("truncates relative time toward zero", () => {
    expect(formatRelative(new Date(now - 90_000), now)).toMatch(/1 minute ago/);
  });
});

describe("labels", () => {
  it("pluralizes with delimiters", () => {
    expect(pluralize(1, "principal")).toBe("1 principal");
    expect(pluralize(1200, "principal")).toBe("1,200 principals");
  });

  it("falls back from name to foreign id to id", () => {
    expect(displayLabel({ id: "prn_1", name: null, foreign_id: "fid" })).toBe("fid");
    expect(displayLabel({ id: "prn_1", name: "", foreign_id: null })).toBe("prn_1");
  });
});
