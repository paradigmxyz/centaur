export function statusLabel(status: string): string {
  return status.replaceAll("_", " ");
}

export const workflowPath = (key: string) => `/console/workflows/${encodeURIComponent(key)}`;

export const WORKFLOW_DB_UNAVAILABLE =
  "Workflow database is unavailable. Console needs the API workflow read-only views to show runs.";
