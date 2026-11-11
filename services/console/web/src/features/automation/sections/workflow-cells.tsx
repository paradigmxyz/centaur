// Table cells shared by the workflow index and detail pages.
import { StatusBadge, workflowStatusTone } from "@/components/badges";
import { LocalTime } from "@/components/local-time";

import type { WorkflowTiming } from "../api";
import { statusLabel } from "./workflow-format";

export function WorkflowStatusPill({ status }: { status: string }) {
  return <StatusBadge tone={workflowStatusTone(status)}>{statusLabel(status)}</StatusBadge>;
}

/** "1 / 3" or "2 / unlimited". */
export function Attempts({ run }: { run: Pick<WorkflowTiming, "attempts" | "max_attempts"> }) {
  return (
    <>
      {run.attempts}
      <span className="text-subtle-foreground"> / {run.max_attempts ?? "unlimited"}</span>
    </>
  );
}

/** Relative start time, plus "queued 4m" when the run waited in the queue. */
export function Started({ run }: { run: Pick<WorkflowTiming, "started_at" | "created_at"> }) {
  return (
    <>
      <LocalTime value={run.started_at} format="relative" />
      {run.created_at && run.started_at !== run.created_at && (
        <div className="mt-1 text-subtle-foreground">
          queued <LocalTime value={run.created_at} format="compact" />
        </div>
      )}
    </>
  );
}
