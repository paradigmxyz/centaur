import type { DiscordbotFetch } from "./types";
import { sliceSurrogateSafe } from "./utils";

// A "status" mention answers directly from the control plane — no sandbox, no
// session turn — so it still works when the agent pipeline is what's broken.
// All data comes from api-rs over HTTP: /healthz + /readyz for liveness, and
// the read-only /api/status report (api-rs owns the session schema; this
// service deliberately runs no SQL). Every fetch carries a timeout and is
// best-effort, so one dead dependency never blanks the rest of the report or
// hangs the per-thread handler lock.

const KEYWORD = /^(status|health)[?!.]*$/i;
// Raw Discord mention markup (<@123>, <@!123>, <@&role>, <#channel>) plus the
// adapter's rewritten form (`@name`): none of it counts as words.
const MENTION_TOKEN = /^(<[@#][!&]?\w+>|@[\w.-]+)$/;

/**
 * True when the message is ONLY a status request ("@bot status",
 * "<@&123> health?"). Anything with more words ("status of the deploy") falls
 * through to a normal agent turn so real questions are never hijacked.
 */
export function isStatusCommand(text: string): boolean {
  const words = text
    .split(/\s+/)
    .filter((word) => word.length > 0 && !MENTION_TOKEN.test(word));
  return words.length === 1 && KEYWORD.test(words[0] ?? "");
}

export type ExecutionRow = {
  ageSeconds: number | null;
  durationSeconds: number | null;
  error: string;
  status: string;
  threadKey: string;
  /** Session title (the conversation name the bots set), when present. */
  title: string;
  /** Display name of whoever triggered the turn, when recorded. */
  who: string;
};

export type DailyRow = {
  /** UTC calendar date, `YYYY-MM-DD`. */
  day: string;
  failed: number;
  runs: number;
};

export type StatusReport = {
  /** null = unreachable, false = responded unhealthy, true = healthy. */
  apiHealthy: boolean | null;
  apiReady: boolean | null;
  daily: DailyRow[];
  inFlight: ExecutionRow[];
  recent: ExecutionRow[];
  /** Whether the /api/status report fetch succeeded. */
  reportOk: boolean;
  sandboxes: { idleSeconds: number | null; sandboxId: string; threadKey: string }[];
  tally: Record<string, number>;
  warmPool: Record<string, number>;
};

const HEALTH_TIMEOUT_MS = 2_000;
const REPORT_TIMEOUT_MS = 5_000;

export async function collectStatus(input: {
  /** Bearer for /api/status (the protected api router); healthz/readyz are open. */
  apiKey?: string;
  apiUrl: string;
  fetchFn?: DiscordbotFetch;
  nowMs?: number;
}): Promise<StatusReport> {
  const fetchFn = input.fetchFn ?? fetch;
  const now = input.nowMs ?? Date.now();

  // null = unreachable (nothing answered), false = answered non-2xx.
  const probe = async (path: string): Promise<boolean | null> => {
    try {
      const response = await fetchFn(`${input.apiUrl}${path}`, {
        signal: AbortSignal.timeout(HEALTH_TIMEOUT_MS),
      });
      return response.ok;
    } catch {
      return null;
    }
  };

  const fetchReport = async (): Promise<Record<string, unknown> | null> => {
    try {
      const response = await fetchFn(`${input.apiUrl}/api/status`, {
        headers: input.apiKey
          ? { authorization: `Bearer ${input.apiKey}` }
          : undefined,
        signal: AbortSignal.timeout(REPORT_TIMEOUT_MS),
      });
      if (!response.ok) return null;
      const body: unknown = await response.json();
      return typeof body === "object" && body !== null
        ? (body as Record<string, unknown>)
        : null;
    } catch {
      return null;
    }
  };

  const [apiHealthy, apiReady, report] = await Promise.all([
    probe("/healthz"),
    probe("/readyz"),
    fetchReport(),
  ]);

  const rows = (key: string): Record<string, unknown>[] => {
    const value = report?.[key];
    return Array.isArray(value)
      ? value.filter(
          (row): row is Record<string, unknown> =>
            typeof row === "object" && row !== null,
        )
      : [];
  };

  const toExecutionRow = (row: Record<string, unknown>): ExecutionRow => ({
    ageSeconds: numberOrNull(row.age_seconds),
    durationSeconds: numberOrNull(row.duration_seconds),
    error: String(row.error ?? ""),
    status: String(row.status ?? "unknown"),
    threadKey: String(row.thread_key ?? "?"),
    title: String(row.title ?? ""),
    who: String(row.user_name ?? ""),
  });

  return {
    apiHealthy,
    apiReady,
    daily: zeroFilledWeek(rows("daily"), now),
    inFlight: rows("in_flight").map(toExecutionRow),
    recent: rows("recent_executions").map(toExecutionRow),
    reportOk: report !== null,
    sandboxes: rows("active_sandboxes").map((row) => ({
      idleSeconds: numberOrNull(row.idle_seconds),
      sandboxId: String(row.sandbox_id ?? "?"),
      threadKey: String(row.thread_key ?? "?"),
    })),
    tally: countsByStatus(rows("tally_24h")),
    warmPool: countsByStatus(rows("warm_pool")),
  };
}

// Discord caps messages at 2000 chars; stay under it with honest truncation.
const STATUS_MAX_CHARS = 1_900;

// Short ASCII tags: emoji are double-width in Discord's code blocks and wreck
// column alignment, which is the whole point of the tabular layout.
const STATUS_TAG: Record<string, string> = {
  cancelled: "cxl",
  completed: "ok",
  failed: "FAIL",
  queued: "que",
  running: "run",
};

const TAG_WIDTH = 5;
const THREAD_WIDTH = 24;
const WHO_WIDTH = 10;
const AGE_WIDTH = 4;
const DUR_WIDTH = 5;
const ERROR_LINE_CHARS = 60;
const BAR_WIDTH = 16;
const RECENT_ROWS_SHOWN = 8;

const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

function weekdayLabel(dayIso: string): string {
  const parsed = new Date(`${dayIso}T00:00:00Z`);
  const label = WEEKDAYS[parsed.getUTCDay()];
  return label ?? "???";
}

/**
 * Discord has no table markup; the closest thing is a monospace code block
 * with hand-padded columns. Header line stays OUTSIDE the block (bold + emoji
 * work there); rows stay ~50 chars wide to limit wrapping on mobile. The
 * histogram gets its own second block. `botName` labels the header — this is
 * generic service code, so the deployment's bot name is a parameter.
 */
export function formatStatus(report: StatusReport, botName: string): string {
  const mark = (value: boolean | null): string =>
    value === null ? "❓" : value ? "✅" : "❌";
  const header =
    `**${botName} status** · api-rs ${mark(report.apiHealthy)} ` +
    `ready ${mark(report.apiReady)} · data ${report.reportOk ? "✅" : "❌"}`;

  const lines: string[] = [];

  const tallyEntries = Object.entries(report.tally).sort();
  if (tallyEntries.length > 0) {
    lines.push(
      `24h: ${tallyEntries
        .map(([status, count]) => `${count} ${STATUS_TAG[status] ?? status}`)
        .join(" · ")}`,
    );
    lines.push("");
  }

  const tableRow = (
    tag: string,
    thread: string,
    who: string,
    age: string,
    took: string,
  ): string =>
    `${tag.padEnd(TAG_WIDTH)} ${fit(thread, THREAD_WIDTH)} ` +
    `${fit(who, WHO_WIDTH, "head")} ${age.padStart(AGE_WIDTH)} ` +
    `${took.padStart(DUR_WIDTH)}`;

  // One table: in-flight turns first (no duration yet), then settled recent
  // turns. The recent list also carries queued/running rows — skip those so
  // an in-flight turn isn't listed twice. AGE = when the turn was requested,
  // TOOK = how long it ran.
  const turnRow = (row: ExecutionRow): string =>
    tableRow(
      STATUS_TAG[row.status] ?? row.status,
      inline(threadLabel(row)),
      inline(row.who),
      formatAge(row.ageSeconds),
      row.durationSeconds !== null ? formatDuration(row.durationSeconds) : "-",
    ).trimEnd();
  const settled = report.recent
    .filter((row) => row.status !== "queued" && row.status !== "running")
    .slice(0, RECENT_ROWS_SHOWN);
  const turns = [...report.inFlight, ...settled];
  if (turns.length > 0) {
    lines.push(tableRow("", "THREAD", "WHO", "AGE", "TOOK").trimEnd());
    for (const row of turns) {
      lines.push(turnRow(row));
      if (row.error) {
        lines.push(`      └ ${inline(row.error).slice(0, ERROR_LINE_CHARS)}`);
      }
    }
  }

  const sandboxBits: string[] = [];
  if (report.sandboxes.length > 0) {
    sandboxBits.push(`${report.sandboxes.length} active`);
  }
  // ready/evicting are the pool's current state; claimed/failed rows are
  // historical (the report windows them to 24h). "failed" here is a warm
  // SPAWN failure (a standby sandbox that didn't provision — the next session
  // cold-starts instead), NOT a failed turn; label it so it can't be confused
  // with the histogram's FAIL column.
  const WARM_LABEL: Record<string, string> = { failed: "spawn-failed" };
  const warmLine = (statuses: string[]): string =>
    statuses
      .filter((status) => (report.warmPool[status] ?? 0) > 0)
      .map(
        (status) => `${report.warmPool[status]} ${WARM_LABEL[status] ?? status}`,
      )
      .join(", ");
  const warmNow = warmLine(["ready", "evicting"]);
  const warmChurn = warmLine(["claimed", "failed"]);
  if (warmNow) sandboxBits.push(`warm: ${warmNow}`);
  if (warmChurn) sandboxBits.push(`warm 24h: ${warmChurn}`);
  if (sandboxBits.length > 0) {
    lines.push("");
    lines.push(`sandboxes: ${sandboxBits.join(" · ")}`);
  }

  if (!report.reportOk) {
    lines.push("! status report unavailable — turn history not shown");
  }

  // 7-day histogram, in its OWN code block below the live view: the bar
  // encodes ONE measure (runs); failures get their own labeled column rather
  // than a second scale or color-alone marking; the failure rate is a plain
  // stat line.
  const histogramLines: string[] = [];
  const week = report.daily;
  const totalRuns = week.reduce((sum, day) => sum + day.runs, 0);
  if (totalRuns > 0) {
    const totalFailed = week.reduce((sum, day) => sum + day.failed, 0);
    const maxRuns = Math.max(...week.map((day) => day.runs));
    histogramLines.push(`     ${"LAST 7 DAYS".padEnd(BAR_WIDTH + 1)}RUNS FAIL`);
    for (const day of week) {
      const bar = "█".repeat(
        day.runs === 0
          ? 0
          : Math.max(1, Math.round((day.runs / maxRuns) * BAR_WIDTH)),
      );
      const fail = day.failed > 0 ? String(day.failed) : "-";
      histogramLines.push(
        `${weekdayLabel(day.day)}  ${bar.padEnd(BAR_WIDTH + 1)}` +
          `${String(day.runs).padStart(4)} ${fail.padStart(4)}`,
      );
    }
    const rate = (totalFailed / totalRuns) * 100;
    histogramLines.push(
      `7d: ${totalRuns} runs · ${totalFailed} failed (${rate.toFixed(1)}%)`,
    );
  }
  const histogram = histogramLines.join("\n");
  const histogramBlock = histogram ? `\n\`\`\`\n${histogram}\n\`\`\`` : "";

  if (lines.length === 0 && !histogramBlock) return header;
  const body = lines.join("\n");
  // The histogram block is small and fixed-size; give the live view whatever
  // budget remains under Discord's cap.
  const budget = STATUS_MAX_CHARS - header.length - histogramBlock.length - 20;
  const bounded =
    body.length <= budget
      ? body
      : `${sliceSurrogateSafe(body, budget - 12).trimEnd()}\n[truncated]`;
  const liveBlock = lines.length > 0 ? `\n\`\`\`\n${bounded}\n\`\`\`` : "";
  return `${header}${liveBlock}${histogramBlock}`;
}

/** Generic failure reply — internals go to logs, not the channel. */
export const STATUS_FAILURE_REPLY = "⚠️ status check failed — see service logs.";

/**
 * Human label for a turn: the session title when the bots set one, otherwise
 * a friendlier rendering of the thread key ("GH PR splits-teams#1799" beats
 * "github-manage:0xSplits/splits-teams:1799"; raw Discord ids stay raw).
 */
function threadLabel(row: { threadKey: string; title: string }): string {
  if (row.title.trim()) return row.title.trim();
  const parts = row.threadKey.split(":");
  const platform = parts[0] ?? row.threadKey;
  const rest = parts.slice(1).join(":");
  if (platform === "github-manage" && parts.length >= 3) {
    const repo = (parts[1] ?? "").split("/").pop() ?? parts[1];
    return `GH PR ${repo}#${parts[2]}`;
  }
  if (platform.startsWith("github")) return `GH ${rest}`;
  if (platform === "linear") return `Linear ${rest}`;
  if (platform === "slack") return `Slack ${rest}`;
  if (platform === "discord") return `Discord ${rest}`;
  return row.threadKey;
}

/**
 * Neutralize markdown/code-fence breakouts in interpolated values (titles and
 * error strings are user/agent-influenced): backticks become apostrophes and
 * whitespace collapses to single spaces so a value can never close the
 * surrounding fence or smuggle its own line.
 */
function inline(value: string): string {
  return value.replace(/`/g, "'").replace(/\s+/g, " ").trim();
}

/**
 * Truncate + pad to the column. Middle ellipsis by default so both ends stay
 * readable ("GH PR splits-con…eams#1799", "Discord 90294…:1391220231" — the
 * head names the thing, the tail discriminates); plain head-cut for names.
 */
function fit(
  value: string,
  width: number,
  keep: "edges" | "head" = "edges",
): string {
  if (value.length <= width) return value.padEnd(width);
  if (keep === "head" || width < 12) {
    return `${value.slice(0, width - 1)}…`;
  }
  const tail = 7;
  return `${value.slice(0, width - tail - 1)}…${value.slice(-tail)}`;
}

function formatAge(seconds: number | null): string {
  if (seconds === null) return "?";
  return formatDuration(seconds);
}

function formatDuration(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.round(s / 60)}m`;
  if (s < 86400) return `${Math.round(s / 3600)}h`;
  return `${Math.round(s / 86400)}d`;
}

/** The last 7 UTC calendar days (oldest→today), zero-filling days with no runs. */
function zeroFilledWeek(
  rows: Record<string, unknown>[],
  nowMs: number,
): DailyRow[] {
  const byDay = new Map<string, { failed: number; runs: number }>();
  for (const row of rows) {
    byDay.set(String(row.day ?? ""), {
      failed: numberOrNull(row.failed) ?? 0,
      runs: numberOrNull(row.runs) ?? 0,
    });
  }
  const days: DailyRow[] = [];
  for (let offset = 6; offset >= 0; offset -= 1) {
    const day = new Date(nowMs - offset * 86_400_000)
      .toISOString()
      .slice(0, 10);
    days.push({ day, failed: 0, runs: 0, ...byDay.get(day) });
  }
  return days;
}

function countsByStatus(
  rows: Record<string, unknown>[],
): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const row of rows) {
    const count = numberOrNull(row.count);
    if (count !== null) counts[String(row.status ?? "unknown")] = count;
  }
  return counts;
}

function numberOrNull(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && value.trim() !== "") {
    const parsed = Number(value);
    if (Number.isFinite(parsed)) return parsed;
  }
  return null;
}
