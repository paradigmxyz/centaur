import { Link } from "react-router";

import { ErrorAlert, WarningAlert } from "@/components/alerts";
import { DataTable, type Column } from "@/components/data-table";
import { LocalTime } from "@/components/local-time";
import { useDocumentTitle } from "@/components/page-header";
import { usePageParam } from "@/components/pagination";
import { RequireAccess } from "@/components/require-access";
import { ApiError, errorMessage } from "@/lib/api";
import { truncateMiddle } from "@/lib/format";

import { useWorkflows, type WorkflowRow } from "../api";
import { Attempts, Started, WorkflowStatusPill } from "../sections/workflow-cells";
import { WORKFLOW_DB_UNAVAILABLE, workflowPath } from "../sections/workflow-format";

export default function WorkflowsListPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.workflows}>
      <WorkflowsList />
    </RequireAccess>
  );
}

const columns: Column<WorkflowRow>[] = [
  {
    id: "workflow",
    header: "Workflow",
    cell: (row) => (
      <>
        {row.workflow_key ? (
          <Link
            to={workflowPath(row.workflow_key)}
            title={row.label}
            className="font-medium text-heading hover:text-centaur-300 hover:underline"
          >
            {truncateMiddle(row.label, 48)}
          </Link>
        ) : (
          <div className="font-medium text-heading" title={row.label}>
            {truncateMiddle(row.label, 48)}
          </div>
        )}
        <div className="mt-1 text-xs text-subtle-foreground">{row.task_name || "workflow"}</div>
      </>
    ),
  },
  {
    id: "status",
    header: "Status",
    className: "whitespace-nowrap text-xs text-muted-foreground",
    cell: (row) => (
      <>
        <WorkflowStatusPill status={row.status} />
        {row.queues.length <= 1 ? (
          <div className="mt-1.5 text-muted-foreground">{row.queue_label}</div>
        ) : (
          <div className="mt-2 space-y-2.5">
            {row.queues.map((queue, index) => (
              <div key={queue.queue_name ?? index} className="flex items-center gap-2">
                <span className="text-subtle-foreground">{index === row.queues.length - 1 ? "└" : "├"}</span>
                <span>{queue.queue_label}</span>
                <WorkflowStatusPill status={queue.status} />
                <span className="text-subtle-foreground">
                  <LocalTime value={queue.last_activity_at} format="compact" /> · {queue.run_count.toLocaleString("en-US")}{" "}
                  runs
                </span>
              </div>
            ))}
          </div>
        )}
      </>
    ),
  },
  {
    id: "attempts",
    header: "Attempts",
    className: "whitespace-nowrap text-xs text-muted-foreground",
    cell: (row) => <Attempts run={row} />,
  },
  {
    id: "started",
    header: "Started",
    className: "whitespace-nowrap text-xs text-muted-foreground",
    cell: (row) => <Started run={row} />,
  },
  {
    id: "finished",
    header: "Finished",
    className: "whitespace-nowrap text-xs text-muted-foreground",
    cell: (row) => <LocalTime value={row.finished_at} format="relative" />,
  },
];

function WorkflowsList() {
  useDocumentTitle("Workflows");
  const [page, setPage] = usePageParam();
  const list = useWorkflows(page);
  const unavailable = list.error instanceof ApiError && list.error.reason === "workflow_database_unavailable";

  return (
    <>
      <h1 className="sr-only">Workflows</h1>
      <h2 className="mb-3 text-sm font-semibold text-heading">Workflow Runs</h2>
      {unavailable && <WarningAlert className="mb-4 px-4 py-3">{WORKFLOW_DB_UNAVAILABLE}</WarningAlert>}
      {list.isError && !unavailable ? (
        <ErrorAlert>Could not load workflows: {errorMessage(list.error)}</ErrorAlert>
      ) : (
        <DataTable
          caption="Workflow runs"
          columns={columns}
          rows={unavailable ? [] : list.data?.data}
          loading={list.isLoading}
          rowKey={(row) => row.workflow_key ?? row.label}
          empty={unavailable ? "No workflow runs available." : "No workflow runs yet."}
          pagination={list.data && !list.isError ? { meta: list.data.meta, unit: "workflow", onPageChange: setPage } : undefined}
        />
      )}
    </>
  );
}
