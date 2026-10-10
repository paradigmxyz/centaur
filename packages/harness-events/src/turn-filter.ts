type JsonRecord = Record<string, unknown>;

function isRecord(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringAtPath(value: unknown, path: string[]): string | undefined {
  let current: unknown = value;
  for (const key of path) {
    if (!isRecord(current)) return undefined;
    current = current[key];
  }
  if (typeof current !== "string") return undefined;
  const trimmed = current.trim();
  return trimmed ? trimmed : undefined;
}

export function turnIds(value: unknown): string[] {
  const paths: string[][] = [
    ["turn_id"],
    ["turnId"],
    ["turn", "id"],
    ["params", "turnId"],
    ["params", "turn", "id"],
  ];
  const ids: string[] = [];
  for (const path of paths) {
    const id = stringAtPath(value, path);
    if (id) ids.push(id);
  }
  return ids;
}

export function threadIds(value: unknown): string[] {
  const paths: string[][] = [
    ["thread_id"],
    ["threadId"],
    ["thread", "id"],
    ["params", "threadId"],
    ["params", "thread_id"],
    ["params", "thread", "id"],
  ];
  const ids: string[] = [];
  for (const path of paths) {
    const id = stringAtPath(value, path);
    if (id) ids.push(id);
  }
  return ids;
}

export function isTurnStartedNotification(value: unknown): boolean {
  if (!isRecord(value)) return false;
  return value.method === "turn/started" || value.type === "turn.started";
}

export function isTurnTerminalNotification(value: unknown): boolean {
  if (!isRecord(value)) return false;
  return (
    value.method === "turn/completed" ||
    value.method === "turn/failed" ||
    value.type === "turn.completed" ||
    value.type === "turn.failed"
  );
}

/**
 * Distinguishes a stream's root turn from multiplexed subagent turns: only
 * the root turn's completion is terminal. A completion counts as a child's
 * only when its ids were seen on a prior `turn/started` and differ from the
 * root; anything else stays terminal.
 */
export class TurnCompletionFilter {
  private rootThreadIds = new Set<string>();
  private rootTurnIds = new Set<string>();
  private seenThreadIds = new Set<string>();
  private seenTurnIds = new Set<string>();
  private rootSeen = false;

  noteLine(value: unknown): void {
    if (!isTurnStartedNotification(value)) return;
    for (const id of threadIds(value)) this.seenThreadIds.add(id);
    for (const id of turnIds(value)) this.seenTurnIds.add(id);
    if (this.rootSeen) return;
    for (const id of threadIds(value)) this.rootThreadIds.add(id);
    for (const id of turnIds(value)) this.rootTurnIds.add(id);
    this.rootSeen = true;
  }

  isChildTurnLine(value: unknown): boolean {
    if (!this.rootSeen) return false;
    const candidateTurns = turnIds(value);
    if (candidateTurns.length > 0 && this.rootTurnIds.size > 0) {
      return (
        !candidateTurns.some((id) => this.rootTurnIds.has(id)) &&
        candidateTurns.some((id) => this.seenTurnIds.has(id))
      );
    }
    const candidateThreads = threadIds(value);
    if (candidateThreads.length > 0 && this.rootThreadIds.size > 0) {
      return (
        !candidateThreads.some((id) => this.rootThreadIds.has(id)) &&
        candidateThreads.some((id) => this.seenThreadIds.has(id))
      );
    }
    return false;
  }

  isChildTurnCompletion(value: unknown): boolean {
    return isTurnTerminalNotification(value) && this.isChildTurnLine(value);
  }
}
