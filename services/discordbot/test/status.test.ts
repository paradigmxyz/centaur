import { describe, expect, it } from "bun:test";
import {
  collectStatus,
  formatStatus,
  isStatusCommand,
  type StatusReport,
} from "../src/status";
import type { DiscordbotFetch } from "../src/types";

describe("isStatusCommand", () => {
  it("matches bare status/health requests with mention markup", () => {
    expect(isStatusCommand("status")).toBe(true);
    expect(isStatusCommand("Status?")).toBe(true);
    expect(isStatusCommand("health!")).toBe(true);
    expect(isStatusCommand("<@123456> status")).toBe(true);
    expect(isStatusCommand("<@!123456> health")).toBe(true);
    expect(isStatusCommand("<@&987> status")).toBe(true);
    expect(isStatusCommand("@gerard status")).toBe(true);
    expect(isStatusCommand("  @gerard   STATUS  ")).toBe(true);
  });

  it("rejects real questions and ordinary messages", () => {
    expect(isStatusCommand("status of the deploy")).toBe(false);
    expect(isStatusCommand("@gerard what's the status?")).toBe(false);
    expect(isStatusCommand("can you check the health of api-rs")).toBe(false);
    expect(isStatusCommand("hello")).toBe(false);
    expect(isStatusCommand("")).toBe(false);
    expect(isStatusCommand("<@123456>")).toBe(false);
  });
});

const NOW = Date.parse("2026-08-12T12:00:00Z");

const FULL_REPORT = {
  ok: true,
  recent_executions: [
    {
      age_seconds: 300,
      duration_seconds: 63,
      error: null,
      status: "completed",
      thread_key: "github-manage:0xSplits/splits-teams:1799",
      title: null,
      user_name: "0xdiid",
    },
    {
      age_seconds: 1900,
      duration_seconds: 12,
      error: "sandbox spawn timeout after 120s",
      status: "failed",
      thread_key: "discord:1:2:9",
      title: null,
      user_name: "jaan",
    },
  ],
  in_flight: [
    {
      age_seconds: 120,
      duration_seconds: null,
      error: null,
      status: "running",
      thread_key: "discord:1:2:3",
      title: "fix the deploy pipeline",
      user_name: "oliver",
    },
  ],
  tally_24h: [
    { count: 41, status: "completed" },
    { count: 2, status: "failed" },
  ],
  active_sandboxes: [
    {
      idle_seconds: 60,
      sandbox_id: "asbx-1755000000-1",
      thread_key: "discord:1:2:3",
    },
  ],
  warm_pool: [
    { count: 2, status: "ready" },
    { count: 41, status: "claimed" },
  ],
  daily: [
    { day: "2026-08-10", failed: 0, runs: 12 },
    { day: "2026-08-11", failed: 3, runs: 40 },
    { day: "2026-08-12", failed: 0, runs: 20 },
  ],
};

function apiFetch(input: {
  health?: number;
  ready?: number;
  report?: unknown;
  reportStatus?: number;
}): DiscordbotFetch {
  return async (url) => {
    const path = String(url);
    if (path.endsWith("/healthz")) {
      return new Response("{}", { status: input.health ?? 200 });
    }
    if (path.endsWith("/readyz")) {
      return new Response("{}", { status: input.ready ?? 200 });
    }
    if (path.endsWith("/api/status")) {
      return new Response(JSON.stringify(input.report ?? FULL_REPORT), {
        status: input.reportStatus ?? 200,
      });
    }
    throw new Error(`unexpected fetch: ${path}`);
  };
}

describe("collectStatus", () => {
  it("assembles a full report when everything is up", async () => {
    const report = await collectStatus({
      apiUrl: "http://api",
      fetchFn: apiFetch({}),
      nowMs: NOW,
    });
    expect(report.apiHealthy).toBe(true);
    expect(report.apiReady).toBe(true);
    expect(report.reportOk).toBe(true);
    expect(report.tally).toEqual({ completed: 41, failed: 2 });
    expect(report.recent).toHaveLength(2);
    expect(report.recent[1]?.error).toContain("sandbox spawn timeout");
    expect(report.inFlight).toHaveLength(1);
    expect(report.sandboxes[0]?.sandboxId).toBe("asbx-1755000000-1");
    expect(report.warmPool).toEqual({ claimed: 41, ready: 2 });
    // Zero-filled to exactly 7 UTC days, oldest first, today last.
    expect(report.daily).toHaveLength(7);
    expect(report.daily[0]).toEqual({ day: "2026-08-06", failed: 0, runs: 0 });
    expect(report.daily[5]).toEqual({ day: "2026-08-11", failed: 3, runs: 40 });
    expect(report.daily[6]).toEqual({ day: "2026-08-12", failed: 0, runs: 20 });
  });

  it("sends the api key as a bearer on the report fetch only", async () => {
    const authByPath = new Map<string, string | undefined>();
    const fetchFn: DiscordbotFetch = async (url, init) => {
      const path = String(url);
      const headers = new Headers(init?.headers);
      authByPath.set(path, headers.get("authorization") ?? undefined);
      return new Response(JSON.stringify(FULL_REPORT), { status: 200 });
    };
    await collectStatus({
      apiKey: "sekrit",
      apiUrl: "http://api",
      fetchFn,
      nowMs: NOW,
    });
    expect(authByPath.get("http://api/api/status")).toBe("Bearer sekrit");
    expect(authByPath.get("http://api/healthz")).toBeUndefined();
    expect(authByPath.get("http://api/readyz")).toBeUndefined();
  });

  it("marks api-rs unreachable (null) but still parses the report", async () => {
    const fetchFn: DiscordbotFetch = async (url) => {
      const path = String(url);
      if (path.endsWith("/healthz") || path.endsWith("/readyz")) {
        throw new Error("connect ECONNREFUSED");
      }
      return new Response(JSON.stringify(FULL_REPORT), { status: 200 });
    };
    const report = await collectStatus({
      apiUrl: "http://api",
      fetchFn,
      nowMs: NOW,
    });
    expect(report.apiHealthy).toBeNull();
    expect(report.apiReady).toBeNull();
    expect(report.reportOk).toBe(true);
  });

  it("distinguishes unhealthy (false) from unreachable (null)", async () => {
    const report = await collectStatus({
      apiUrl: "http://api",
      fetchFn: apiFetch({ ready: 503 }),
      nowMs: NOW,
    });
    expect(report.apiHealthy).toBe(true);
    expect(report.apiReady).toBe(false);
  });

  it("still reports health when the status report fails", async () => {
    const report = await collectStatus({
      apiUrl: "http://api",
      fetchFn: apiFetch({ reportStatus: 500 }),
      nowMs: NOW,
    });
    expect(report.apiHealthy).toBe(true);
    expect(report.reportOk).toBe(false);
    expect(report.recent).toEqual([]);
    expect(report.daily.every((day) => day.runs === 0)).toBe(true);
  });

  it("tolerates a malformed report body", async () => {
    const report = await collectStatus({
      apiUrl: "http://api",
      fetchFn: apiFetch({ report: "not an object" }),
      nowMs: NOW,
    });
    expect(report.reportOk).toBe(false);
    expect(report.recent).toEqual([]);
  });
});

describe("formatStatus", () => {
  const baseReport = (): StatusReport => ({
    apiHealthy: true,
    apiReady: true,
    daily: [],
    inFlight: [],
    recent: [],
    reportOk: true,
    sandboxes: [],
    tally: {},
    warmPool: {},
  });

  it("renders a code-block table with tags, tallies, and sandboxes", async () => {
    const report = await collectStatus({
      apiUrl: "http://api",
      fetchFn: apiFetch({}),
      nowMs: NOW,
    });
    const text = formatStatus(report, "centaur");
    // Header names the bot and stays outside the block.
    expect(text.startsWith("**centaur status** · api-rs ✅")).toBe(true);
    expect(text).toContain("24h: 41 ok · 2 FAIL");
    // Column headings above the turn table.
    expect(text).toMatch(/THREAD\s+WHO\s+AGE\s+TOOK/);
    // In-flight row first: session title, requester, no duration yet.
    const lines = text.split("\n");
    const runLine = lines.find((line) => line.startsWith("run"));
    expect(runLine).toContain("fix the deploy pipeline");
    expect(runLine).toContain("oliver");
    expect(runLine?.trimEnd().endsWith("-")).toBe(true);
    // Untitled management turn falls back to the friendly PR label.
    expect(text).toContain("GH PR splits-teams#1799");
    expect(text).toMatch(/ok\s+GH PR splits-teams#1799\s+0xdiid\s+5m\s+1m/);
    // Errors land on their own indented line.
    expect(text).toContain("└ sandbox spawn timeout");
    expect(text).toContain(
      "sandboxes: 1 active · warm: 2 ready · warm 24h: 41 claimed",
    );
    // Histogram in its OWN code block, after the live view.
    expect(text.split("```")).toHaveLength(5);
    expect(text.indexOf("LAST 7 DAYS")).toBeGreaterThan(
      text.indexOf("sandboxes:"),
    );
    expect(text).toMatch(/Tue {2}█{16}\s+40\s+3/);
    expect(text).toMatch(/Wed {2}█+\s+20\s+-/);
    expect(text).toMatch(/Thu {2}\s+0\s+-/);
    expect(text).toContain("7d: 72 runs · 3 failed (4.2%)");
    expect(text.length).toBeLessThanOrEqual(2000);
  });

  it("neutralizes backticks and newlines in titles and errors", () => {
    const report = baseReport();
    report.recent = [
      {
        ageSeconds: 60,
        durationSeconds: 5,
        error: "boom ``` **bold**\nnext line",
        status: "failed",
        threadKey: "discord:1:2",
        title: "evil ``` title",
        who: "someone",
      },
    ];
    const text = formatStatus(report, "centaur");
    // Exactly the wrapper's own fence pair — no fences leaked from values.
    expect(text.split("```")).toHaveLength(3);
    expect(text).toContain("evil ''' title");
    expect(text).toContain("boom ''' **bold** next line");
  });

  it("keeps thread rows within the column budget", () => {
    const report = baseReport();
    report.recent = [
      {
        ageSeconds: 60,
        durationSeconds: 30,
        error: "",
        status: "completed",
        threadKey: `discord:${"9".repeat(60)}`,
        title: "",
        who: "someone-with-a-long-name",
      },
    ];
    const text = formatStatus(report, "centaur");
    const row = text.split("\n").find((line) => line.startsWith("ok"));
    expect(row).toBeDefined();
    // Middle ellipsis keeps the platform head and the id tail.
    expect(row).toContain("Discord 9");
    expect(row).toContain("…");
    expect(row).toContain("someone-w…");
    expect(row?.length ?? 0).toBeLessThanOrEqual(52);
  });

  it("marks a down api-rs and missing report honestly", () => {
    const report = baseReport();
    report.apiHealthy = null;
    report.apiReady = false;
    report.reportOk = false;
    const text = formatStatus(report, "centaur");
    expect(text).toContain("api-rs ❓");
    expect(text).toContain("ready ❌");
    expect(text).toContain("data ❌");
    expect(text).toContain("status report unavailable");
  });

  it("stays under the Discord cap with oversized errors", () => {
    const report = baseReport();
    report.recent = Array.from({ length: 30 }, (_, index) => ({
      ageSeconds: 60 * index,
      durationSeconds: 5,
      error: "x".repeat(150),
      status: "failed",
      threadKey: `discord:${"y".repeat(80)}:${index}`,
      title: "",
      who: "someone",
    }));
    const text = formatStatus(report, "centaur");
    expect(text.length).toBeLessThanOrEqual(2000);
    expect(text.endsWith("```")).toBe(true);
  });
});
