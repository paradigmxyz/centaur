import { describe, expect, test } from "bun:test";
import { AsyncTextQueue, sliceSurrogateSafe } from "../src/utils";

describe("sliceSurrogateSafe", () => {
  test("returns short strings unchanged", () => {
    expect(sliceSurrogateSafe("abc", 10)).toBe("abc");
  });

  test("cuts at the limit outside surrogate pairs", () => {
    expect(sliceSurrogateSafe("abcdef", 3)).toBe("abc");
  });

  test("backs off a cut that would split a surrogate pair", () => {
    const text = "ab\u{1f600}cd"; // 😀 is a surrogate pair at units 2-3
    expect(sliceSurrogateSafe(text, 3)).toBe("ab");
    expect(sliceSurrogateSafe(text, 4)).toBe("ab\u{1f600}");
  });

  test("returns empty for non-positive limits", () => {
    expect(sliceSurrogateSafe("abc", 0)).toBe("");
  });
});

describe("AsyncTextQueue", () => {
  test("drains pushed values then finishes on end()", async () => {
    const queue = new AsyncTextQueue();
    queue.push("a");
    queue.push("b");
    queue.end();
    const seen: string[] = [];
    for await (const value of queue) seen.push(value);
    expect(seen).toEqual(["a", "b"]);
  });

  test("wakes a waiting consumer", async () => {
    const queue = new AsyncTextQueue();
    const seen: string[] = [];
    const consumer = (async () => {
      for await (const value of queue) seen.push(value);
    })();
    queue.push("late");
    queue.end();
    await consumer;
    expect(seen).toEqual(["late"]);
  });
});
