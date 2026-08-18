import { describe, expect, test } from "bun:test";
import { runExclusive } from "../src/context";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

describe("runExclusive", () => {
  test("serializes work sharing a key in arrival order (no interleave)", async () => {
    const order: string[] = [];
    const run = (label: string, delay: number) =>
      runExclusive("same", async () => {
        order.push(`start:${label}`);
        await sleep(delay);
        order.push(`end:${label}`);
      });
    // 'a' is slow; if they interleaved, b/c would start before a ends.
    await Promise.all([run("a", 25), run("b", 1), run("c", 1)]);
    expect(order).toEqual([
      "start:a",
      "end:a",
      "start:b",
      "end:b",
      "start:c",
      "end:c",
    ]);
  });

  test("different keys run concurrently", async () => {
    const order: string[] = [];
    const a = runExclusive("a", async () => {
      order.push("a-start");
      await sleep(20);
      order.push("a-end");
    });
    const b = runExclusive("b", async () => {
      order.push("b-start");
      await sleep(1);
      order.push("b-end");
    });
    await Promise.all([a, b]);
    // b ran alongside a and finished first.
    expect(order.indexOf("b-end")).toBeLessThan(order.indexOf("a-end"));
  });

  test("a thrown error releases the lock for the next queued call", async () => {
    const results: string[] = [];
    const first = runExclusive("k", async () => {
      throw new Error("boom");
    }).catch(() => results.push("first-failed"));
    const second = runExclusive("k", async () => {
      results.push("second-ran");
    });
    await Promise.all([first, second]);
    expect(results).toContain("second-ran");
    expect(results).toContain("first-failed");
  });
});
