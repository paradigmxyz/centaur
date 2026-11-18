export function statusLabel(status: string): string {
  return status.replaceAll("_", " ");
}

export const workflowPath = (key: string) => `/console/workflows/${encodeURIComponent(key)}`;

export const WORKFLOW_DB_UNAVAILABLE =
  "Workflow database is unavailable. Console needs the API workflow read-only views to show runs.";

/** "45s", "3m 20s", "2h 5m", "1d 4h": how long a run took (or "running"). */
export function durationLabel(run: {
  status: string;
  started_at: string | null;
  finished_at: string | null;
}): string {
  if (!run.started_at) return "—";
  if (!run.finished_at) return run.status === "running" ? "running" : "—";
  const seconds = Math.max(0, Math.round((Date.parse(run.finished_at) - Date.parse(run.started_at)) / 1000));
  if (Number.isNaN(seconds)) return "—";
  const [days, hours, minutes] = [Math.floor(seconds / 86400), Math.floor(seconds / 3600) % 24, Math.floor(seconds / 60) % 60];
  if (days > 0) return hours ? `${days}d ${hours}h` : `${days}d`;
  if (hours > 0) return minutes ? `${hours}h ${minutes}m` : `${hours}h`;
  if (minutes > 0) return seconds % 60 ? `${minutes}m ${seconds % 60}s` : `${minutes}m`;
  return `${seconds}s`;
}
