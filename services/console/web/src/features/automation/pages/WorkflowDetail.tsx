import type { ReactNode } from "react";
import { Link, useLocation, useParams, useSearchParams } from "react-router";
import { toast } from "sonner";

import { ErrorAlert, WarningAlert } from "@/components/alerts";
import { BackLink } from "@/components/back-link";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { DataTable, type Column } from "@/components/data-table";
import { EmptyState } from "@/components/empty-state";
import { LocalTime } from "@/components/local-time";
import { SectionHeading, useDocumentTitle } from "@/components/page-header";
import { usePageParam } from "@/components/pagination";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { ApiError, errorMessage } from "@/lib/api";
import { pluralize, truncateMiddle } from "@/lib/format";
import { cn } from "@/lib/utils";

import { useForceStartWorkflow, useWorkflow, useWorkflowRuns, type WorkflowDetail, type WorkflowRun } from "../api";
import { Attempts, Started, WorkflowStatusPill } from "../sections/workflow-cells";
import { statusLabel, WORKFLOW_DB_UNAVAILABLE } from "../sections/workflow-format";

export default function WorkflowDetailPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.workflows}>
      <WorkflowDetailRoute />
    </RequireAccess>
  );
}

function WorkflowDetailRoute() {
  const { id = "" } = useParams();
  const workflow = useWorkflow(id);
  useDocumentTitle(workflow.data?.label ?? id);

  const back = (
    <div className="mb-8">
      <BackLink to="/console/workflows">back to workflows</BackLink>
    </div>
  );

  if (workflow.isLoading) return <p className="text-sm text-muted-foreground">Loading…</p>;
  if (workflow.error instanceof ApiError && workflow.error.reason === "workflow_database_unavailable") {
    return (
      <>
        {back}
        <WarningAlert className="mb-4 px-4 py-3">{WORKFLOW_DB_UNAVAILABLE}</WarningAlert>
      </>
    );
  }
  if (workflow.error instanceof ApiError && workflow.error.status === 404) {
    return (
      <>
        {back}
        <EmptyState>
          No workflow runs found for <span className="text-foreground">{id}</span>.
        </EmptyState>
      </>
    );
  }
  if (!workflow.data) {
    return (
      <>
        {back}
        <ErrorAlert>{errorMessage(workflow.error)}</ErrorAlert>
      </>
    );
  }
  return <WorkflowDashboard workflow={workflow.data} />;
}

function WorkflowDashboard({ workflow }: { workflow: WorkflowDetail }) {
  const key = workflow.workflow_key;
  const forceStart = useForceStartWorkflow(key);
  const schedule = workflow.schedule;
  const totalRuns = pluralize(workflow.run_count, "run");

  const trigger = async () => {
    try {
      const { data } = await forceStart.mutateAsync();
      if (data.created) toast.success(`Run queued (${data.run_id}).`);
      else toast.success(`A run with this idempotency key is already queued (${data.run_id}).`);
    } catch (error) {
      toast.error(`Could not start workflow: ${errorMessage(error)}`);
    }
  };

  const overview: [string, ReactNode][] = [
    ["Queue", workflow.queues.map((queue) => queue.queue_label).join(", ") || workflow.queue_label],
    ["Engine", workflow.engine_label],
    ...(schedule?.summary ? ([["Schedule", schedule.summary]] as [string, ReactNode][]) : []),
    ["Started", workflow.started_at ? <LocalTime value={workflow.started_at} /> : null],
    ["Duration", workflow.duration_label],
    ["Attempts", <Attempts key="attempts" run={workflow} />],
    ["Runs", totalRuns],
  ];

  return (
    <>
      <div className="mb-8">
        <BackLink to="/console/workflows">back to workflows</BackLink>
        <div className="mt-2 flex flex-wrap items-center gap-3">
          <h1 className="text-2xl font-bold text-heading" title={workflow.label}>
            {truncateMiddle(workflow.label, 72)}
          </h1>
          <WorkflowStatusPill status={workflow.status} />
          <div className="ml-auto">
            <ConfirmDialog
              title={`Manually trigger a ${key} run?`}
              description={schedule ? "It starts with the registered schedule input." : undefined}
              confirmLabel="Manually Trigger"
              destructive={false}
              onConfirm={trigger}
              trigger={<Button disabled={forceStart.isPending}>Manually Trigger</Button>}
            />
          </div>
        </div>
        <div className="mt-2 flex flex-wrap items-center gap-2 text-sm">
          <span className="text-muted-foreground">{workflow.task_name || "workflow"}</span>
          <span className="text-subtle-foreground">·</span>
          <span className="text-muted-foreground">{totalRuns}</span>
          {schedule?.source_url && (
            <>
              <span className="text-subtle-foreground">·</span>
              <a
                href={schedule.source_url}
                target="_blank"
                rel="noopener noreferrer"
                title={schedule.source_path ?? undefined}
                className="text-centaur-400 hover:underline light:text-primary"
              >
                {schedule.source_path} ↗
              </a>
            </>
          )}
        </div>
      </div>

      <section className="mb-8">
        <SectionHeading className="mb-3">Overview</SectionHeading>
        <DefinitionList rows={overview.map(([label, value]) => ({ label, value }))} />
      </section>

      {workflow.debug.length > 0 && (
        <section className="mb-8">
          <SectionHeading className="mb-3">Debugging</SectionHeading>
          <DefinitionList
            rows={workflow.debug.map((row) => ({
              label: row.label,
              danger: row.label.includes("ailure"),
              value: (
                <div className="flex-1">
                  {row.run_id && <div className="mb-1 text-muted-foreground">run {row.run_id}</div>}
                  <pre className="max-h-64 overflow-auto rounded-sm bg-muted p-3 leading-5">{debugJson(row.value)}</pre>
                </div>
              ),
            }))}
          />
        </section>
      )}

      <HistoricalRuns workflow={workflow} />
    </>
  );
}

function DefinitionList({ rows }: { rows: { label: string; value: ReactNode; danger?: boolean }[] }) {
  return (
    <dl className="divide-y divide-border/70 rounded-sm border border-border bg-card">
      {rows.map((row) => (
        <div key={row.label} className="flex flex-col gap-1 px-5 py-3 sm:flex-row sm:gap-4">
          <dt
            className={cn(
              "w-48 shrink-0 text-xs tracking-wider uppercase",
              row.danger ? "text-red-400 light:text-red-700" : "text-muted-foreground",
            )}
          >
            {row.label}
          </dt>
          <dd className="min-w-0 flex-1 text-xs text-foreground">
            {row.value === null || row.value === undefined || row.value === "" ? (
              <span className="text-subtle-foreground">—</span>
            ) : (
              row.value
            )}
          </dd>
        </div>
      ))}
    </dl>
  );
}

function debugJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2) ?? String(value);
  } catch {
    return String(value);
  }
}

const runColumns: Column<WorkflowRun>[] = [
  { id: "status", header: "Status", className: "whitespace-nowrap", cell: (run) => <WorkflowStatusPill status={run.status} /> },
  { id: "queue", header: "Queue", className: "whitespace-nowrap text-xs text-muted-foreground", cell: (run) => run.queue_label },
  {
    id: "attempts",
    header: "Attempts",
    className: "whitespace-nowrap text-xs text-muted-foreground",
    cell: (run) => <Attempts run={run} />,
  },
  {
    id: "started",
    header: "Started",
    className: "whitespace-nowrap text-xs text-muted-foreground",
    cell: (run) => <Started run={run} />,
  },
  {
    id: "finished",
    header: "Finished",
    className: "whitespace-nowrap text-xs text-muted-foreground",
    cell: (run) => <LocalTime value={run.finished_at} format="relative" />,
  },
  {
    id: "run",
    header: "Run",
    className: "text-xs text-muted-foreground",
    cell: (run) => (
      <>
        <div title={run.run_id}>{truncateMiddle(run.run_id, 34)}</div>
        {run.task_id && (
          <div className="mt-1 text-subtle-foreground" title={run.task_id}>
            {truncateMiddle(run.task_id, 34)}
          </div>
        )}
      </>
    ),
  },
];

/** Status/queue filter chips + paginated run table. Changing a filter resets the page. */
function HistoricalRuns({ workflow }: { workflow: WorkflowDetail }) {
  const [params] = useSearchParams();
  const { pathname } = useLocation();
  const status = params.get("status") ?? "";
  const queue = params.get("queue") ?? "";
  const [page, setPage] = usePageParam();
  const runs = useWorkflowRuns(workflow.workflow_key, { status, queue, page }, true);

  const filterHref = (next: { status?: string; queue?: string }) => {
    const query = new URLSearchParams();
    const nextStatus = next.status ?? status;
    const nextQueue = next.queue ?? queue;
    if (nextStatus) query.set("status", nextStatus);
    if (nextQueue) query.set("queue", nextQueue);
    const qs = query.toString();
    return qs ? `${pathname}?${qs}` : pathname;
  };

  return (
    <>
      <SectionHeading className="mb-3">Historical Runs</SectionHeading>
      <div className="mb-3 flex flex-wrap items-center gap-1.5">
        <FilterChip to={filterHref({ status: "" })} active={status === ""}>
          all <span className="ml-1 text-muted-foreground">{workflow.run_count.toLocaleString("en-US")}</span>
        </FilterChip>
        {workflow.status_counts.map((entry) => (
          <FilterChip key={entry.status} to={filterHref({ status: entry.status })} active={status === entry.status}>
            {statusLabel(entry.status)}{" "}
            <span className="ml-1 text-muted-foreground">{entry.count.toLocaleString("en-US")}</span>
          </FilterChip>
        ))}
      </div>
      {workflow.queues.length > 1 && (
        <div className="mb-3 flex flex-wrap items-center gap-1.5">
          <span className="mr-1 text-xs tracking-wider text-muted-foreground uppercase">Queue</span>
          <FilterChip to={filterHref({ queue: "" })} active={queue === ""}>
            all
          </FilterChip>
          {workflow.queues.map((entry) => (
            <FilterChip key={entry.queue_name} to={filterHref({ queue: entry.queue_name })} active={queue === entry.queue_name}>
              {entry.queue_label}
            </FilterChip>
          ))}
        </div>
      )}
      <DataTable
        caption="Historical runs"
        columns={runColumns}
        rows={runs.isError ? [] : runs.data?.data}
        loading={runs.isLoading}
        rowKey={(run) => run.run_id}
        empty={runs.isError ? errorMessage(runs.error) : "No runs match the selected filters."}
        pagination={
          runs.data && !runs.isError
            ? { meta: runs.data.meta, unit: "run", onPageChange: setPage }
            : undefined
        }
      />
    </>
  );
}

function FilterChip({ to, active, children }: { to: string; active: boolean; children: ReactNode }) {
  return (
    <Link
      to={to}
      aria-current={active ? "true" : undefined}
      className={cn(
        "inline-flex items-center rounded-sm border px-2 py-1 text-xs transition-colors",
        active
          ? "border-primary/50 bg-primary/10 text-centaur-300 light:text-primary"
          : "border-border text-muted-foreground hover:border-primary/40 hover:text-foreground",
      )}
    >
      {children}
    </Link>
  );
}
