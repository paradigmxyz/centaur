import { describe, expect, it } from "vitest";

import { durationLabel } from "./workflow-format";

const run = (started_at: string | null, finished_at: string | null, status = "completed") => ({
  status,
  started_at,
  finished_at,
});

describe("durationLabel", () => {
  it("formats finished runs", () => {
    expect(durationLabel(run("2026-01-01T00:00:00Z", "2026-01-01T00:00:45Z"))).toBe("45s");
    expect(durationLabel(run("2026-01-01T00:00:00Z", "2026-01-01T00:03:20Z"))).toBe("3m 20s");
    expect(durationLabel(run("2026-01-01T00:00:00Z", "2026-01-01T02:05:00Z"))).toBe("2h 5m");
    expect(durationLabel(run("2026-01-01T00:00:00Z", "2026-01-02T04:00:00Z"))).toBe("1d 4h");
  });

  it("shows running and unknown runs", () => {
    expect(durationLabel(run("2026-01-01T00:00:00Z", null, "running"))).toBe("running");
    expect(durationLabel(run("2026-01-01T00:00:00Z", null, "pending"))).toBe("—");
    expect(durationLabel(run(null, null))).toBe("—");
  });
});
