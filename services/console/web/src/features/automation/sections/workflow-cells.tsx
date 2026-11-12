import { StatusBadge, workflowStatusTone } from "@/components/badges";
import { LocalTime } from "@/components/local-time";

import type { WorkflowTiming } from "../api";
import { statusLabel } from "./workflow-format";

export function WorkflowStatusPill({ status }: { status: string }) {
  return <StatusBadge tone={workflowStatusTone(status)}>{statusLabel(status)}</StatusBadge>;
}

export function Attempts({ run }: { run: Pick<WorkflowTiming, "attempts" | "max_attempts"> }) {
  return (
    <>
      {run.attempts}
      <span className="text-muted-foreground"> / {run.max_attempts ?? "unlimited"}</span>
    </>
  );
}

export function Started({ run }: { run: Pick<WorkflowTiming, "started_at" | "created_at"> }) {
  return (
    <>
      <LocalTime value={run.started_at} format="relative" />
      {run.created_at && run.started_at !== run.created_at && (
        <div className="mt-1 text-muted-foreground">
          queued <LocalTime value={run.created_at} format="compact" />
        </div>
      )}
    </>
  );
}
