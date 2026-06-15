import { describe, expect, it } from "bun:test";
import {
  GuildExecutionLimiter,
  sliceSurrogateSafe,
  splitDiscordMessageChunks,
  takeDiscordMessageChunk,
} from "../src/utils";

describe("sliceSurrogateSafe", () => {
  it("returns the value unchanged when it fits", () => {
    expect(sliceSurrogateSafe("hello", 10)).toBe("hello");
    expect(sliceSurrogateSafe("hello", 5)).toBe("hello");
  });

  it("slices plain text at the limit", () => {
    expect(sliceSurrogateSafe("hello", 3)).toBe("hel");
  });

  it("backs off a cut that lands inside a surrogate pair", () => {
    // "💥" is U+1F4A5: two UTF-16 units. Cutting after the high surrogate
    // would produce an invalid string that Discord rejects with a 400.
    const value = `ab💥cd`;
    expect(sliceSurrogateSafe(value, 3)).toBe("ab");
    expect(sliceSurrogateSafe(value, 4)).toBe("ab💥");
  });

  it("never splits a pair anywhere in an emoji run", () => {
    const value = "💥".repeat(10);
    for (let max = 1; max <= value.length; max++) {
      const sliced = sliceSurrogateSafe(value, max);
      expect(sliced.length % 2).toBe(0);
      expect(sliced).toBe("💥".repeat(sliced.length / 2));
    }
  });

  it("returns empty for a non-positive limit", () => {
    expect(sliceSurrogateSafe("hello", 0)).toBe("");
  });
});

describe("takeDiscordMessageChunk", () => {
  it("returns null when the text already fits", () => {
    expect(takeDiscordMessageChunk("short", 100)).toBeNull();
  });

  it("splits at the latest newline boundary", () => {
    const text = `${"a".repeat(60)}\n${"b".repeat(60)}\n${"c".repeat(60)}`;
    const split = takeDiscordMessageChunk(text, 130);
    expect(split).not.toBeNull();
    expect(split?.chunk).toBe(`${"a".repeat(60)}\n${"b".repeat(60)}`);
    expect(split?.rest).toBe("c".repeat(60));
  });

  it("splits at whitespace when no newline is available", () => {
    const text = `${"a".repeat(80)} ${"b".repeat(80)}`;
    const split = takeDiscordMessageChunk(text, 100);
    expect(split?.chunk).toBe("a".repeat(80));
    expect(split?.rest).toBe("b".repeat(80));
  });

  it("hard-cuts a pathological single line without exceeding the limit", () => {
    const text = "x".repeat(250);
    const split = takeDiscordMessageChunk(text, 100);
    expect(split?.chunk).toBe("x".repeat(100));
    expect(split?.rest).toBe("x".repeat(150));
  });

  it("hard cuts are surrogate-safe", () => {
    const text = "💥".repeat(200);
    const split = takeDiscordMessageChunk(text, 101);
    expect(split).not.toBeNull();
    const chunk = split?.chunk ?? "";
    expect(chunk.length % 2).toBe(0);
    expect(chunk.length).toBeLessThanOrEqual(101);
    expect(`${chunk}${split?.rest}`).toBe(text);
  });

  it("prefers a boundary outside a code fence over a later one inside", () => {
    const prose = "p".repeat(50);
    const fenced = `\`\`\`ts\n${"code\n".repeat(20)}`;
    const text = `${prose}\n${fenced}`;
    const split = takeDiscordMessageChunk(text, 80);
    expect(split?.chunk).toBe(prose);
    expect(split?.rest.startsWith("```ts\n")).toBe(true);
  });

  it("closes and re-opens a fence when a split inside it is unavoidable", () => {
    const lines = Array.from({ length: 40 }, (_, i) => `line-${i}`).join("\n");
    const text = `\`\`\`ts\n${lines}\n\`\`\``;
    const split = takeDiscordMessageChunk(text, 120);
    expect(split).not.toBeNull();
    expect(split?.chunk.endsWith("\n```")).toBe(true);
    expect(split?.chunk.length).toBeLessThanOrEqual(120);
    expect(split?.rest.startsWith("```ts\n")).toBe(true);
  });
});

describe("splitDiscordMessageChunks", () => {
  it("keeps every chunk within the limit and preserves content", () => {
    const paragraphs = Array.from(
      { length: 30 },
      (_, i) => `paragraph ${i} ${"word ".repeat(20)}`,
    ).join("\n\n");
    const chunks = splitDiscordMessageChunks(paragraphs, 200);
    expect(chunks.length).toBeGreaterThan(1);
    for (const chunk of chunks) {
      expect(chunk.length).toBeLessThanOrEqual(200);
      expect(chunk.trim().length).toBeGreaterThan(0);
    }
  });

  it("returns a single chunk for short text", () => {
    expect(splitDiscordMessageChunks("hi", 100)).toEqual(["hi"]);
  });

  it("returns no chunks for whitespace-only text", () => {
    expect(splitDiscordMessageChunks("   \n  ", 100)).toEqual([]);
  });

  it("re-opens fences so each chunk renders as code", () => {
    const text = `\`\`\`py\n${"print('x')\n".repeat(60)}\`\`\``;
    const chunks = splitDiscordMessageChunks(text, 150);
    expect(chunks.length).toBeGreaterThan(1);
    for (const chunk of chunks.slice(0, -1)) {
      expect(chunk.endsWith("```")).toBe(true);
    }
    for (const chunk of chunks.slice(1)) {
      expect(chunk.startsWith("```py\n")).toBe(true);
    }
  });

  it("terminates on emoji-only input", () => {
    const chunks = splitDiscordMessageChunks("💥".repeat(500), 99);
    expect(chunks.join("")).toBe("💥".repeat(500));
    for (const chunk of chunks) {
      expect(chunk.length).toBeLessThanOrEqual(99);
      expect(chunk.length % 2).toBe(0);
    }
  });
});

describe("GuildExecutionLimiter", () => {
  it("caps acquisitions per guild and releases idempotently", () => {
    const limiter = new GuildExecutionLimiter(2);
    const first = limiter.acquire("G1");
    const second = limiter.acquire("G1");
    expect(first).not.toBeNull();
    expect(second).not.toBeNull();
    expect(limiter.acquire("G1")).toBeNull();
    // Other guilds are unaffected.
    expect(limiter.acquire("G2")).not.toBeNull();

    first?.();
    first?.(); // double release must not free a second slot
    expect(limiter.inFlight("G1")).toBe(1);
    expect(limiter.acquire("G1")).not.toBeNull();
    expect(limiter.acquire("G1")).toBeNull();
  });
});
