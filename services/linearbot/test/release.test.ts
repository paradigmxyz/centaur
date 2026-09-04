import { describe, expect, it } from "bun:test";

import { applyRelease } from "../src/index";
import type { LinearbotOptions, LinearbotTrace } from "../src/types";

const THREAD = "linear:issue-1";
const trace: LinearbotTrace = {
  includeContext: false,
  messageId: "release-issue-1",
  mode: "execute",
  openStream: false,
  startedAtMs: 0,
  threadId: THREAD,
};

// Linear `updatedAt` stamps. The entry the release reads was created by an
// assignment processed at ASSIGN_UPDATED_AT; a release after it is fresh, one
// before it is a take-back the re-delegation already superseded.
const ASSIGN_UPDATED_AT = "2026-06-17T00:05:00.000Z";
const FRESH_RELEASE = "2026-06-17T00:10:00.000Z";
const STALE_RELEASE = "2026-06-17T00:01:00.000Z";

function releaseOptions(calls: { method: string; url: string }[]): LinearbotOptions {
  return {
    apiUrl: "http://localhost",
    fetch: async (url: string, init?: { method?: string }) => {
      calls.push({ method: init?.method ?? "GET", url: String(url) });
      return new Response(JSON.stringify({ interrupted: true }), {
        headers: { "content-type": "application/json" },
        status: 200,
      });
    },
  } as unknown as LinearbotOptions;
}

function pendingEntry() {
  return {
    released: false,
    started: false,
    assignmentUpdatedAtMs: Date.parse(ASSIGN_UPDATED_AT),
  };
}

describe("applyRelease", () => {
  it("interrupts even when the local pending entry says the turn has not started", async () => {
    // A newer assignment overwrote the map, so the entry this release reads
    // says `started: false` -- yet a turn from the evicted, earlier handoff
    // may still be streaming on the now-taken-back issue. Gating the interrupt
    // on that flag is the race that lets work continue, so it must be attempted
    // regardless.
    const calls: { method: string; url: string }[] = [];
    const pending = pendingEntry();
    await applyRelease(
      releaseOptions(calls),
      THREAD,
      pending,
      trace,
      "issue-1",
      FRESH_RELEASE,
    );

    expect(calls).toEqual([
      {
        method: "POST",
        url: "http://localhost/api/session/linear%3Aissue-1/interrupt",
      },
    ]);
    // A still-queued turn is separately marked so it will not start.
    expect(pending.released).toBe(true);
    expect(pending.started).toBe(false);
  });

  it("interrupts with no local pending entry at all", async () => {
    // A turn started before a restart leaves no local entry; the interrupt is
    // the only thing that can still reach it.
    const calls: { method: string; url: string }[] = [];
    await applyRelease(
      releaseOptions(calls),
      THREAD,
      undefined,
      trace,
      "issue-1",
      FRESH_RELEASE,
    );
    expect(calls).toEqual([
      {
        method: "POST",
        url: "http://localhost/api/session/linear%3Aissue-1/interrupt",
      },
    ]);
  });

  it("skips a stale release that predates the current assignment", async () => {
    // The entry this release reads belongs to a re-delegation that happened
    // *after* the take-back (a redelivered/out-of-order webhook). Interrupting
    // would kill that turn and marking it released would drop it before it
    // starts, so both are skipped.
    const calls: { method: string; url: string }[] = [];
    const pending = pendingEntry();
    await applyRelease(
      releaseOptions(calls),
      THREAD,
      pending,
      trace,
      "issue-1",
      STALE_RELEASE,
    );

    expect(calls).toEqual([]);
    expect(pending.released).toBe(false);
    expect(pending.started).toBe(false);
  });

  it("interrupts when the release carries no updatedAt to compare against", async () => {
    // No timestamp means the release cannot be told stale, so the safe default
    // is to interrupt: a genuine take-back must still reach the turn.
    const calls: { method: string; url: string }[] = [];
    const pending = pendingEntry();
    await applyRelease(releaseOptions(calls), THREAD, pending, trace, "issue-1", "");

    expect(calls).toEqual([
      {
        method: "POST",
        url: "http://localhost/api/session/linear%3Aissue-1/interrupt",
      },
    ]);
    expect(pending.released).toBe(true);
  });
});
