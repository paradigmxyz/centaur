import { Clock } from "lucide-react";
import { Link } from "react-router";
import { toast } from "sonner";

import { ErrorAlert } from "@/components/alerts";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { DataTable, type Column } from "@/components/data-table";
import { LocalTime } from "@/components/local-time";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";
import { cn } from "@/lib/utils";

import { useScheduledTaskMutations, useScheduledTasks, type ScheduledTask } from "../api";

export default function ScheduledTasksListPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.scheduled_tasks}>
      <ScheduledTasksList />
    </RequireAccess>
  );
}

const editPath = (task: ScheduledTask) => `/console/scheduled_tasks/${task.id}/edit`;

function ScheduledTasksList() {
  const tasks = useScheduledTasks();
  const mutations = useScheduledTaskMutations();

  const run = async (task: ScheduledTask) => {
    try {
      await mutations.run.mutateAsync(task.id ?? "");
      toast.success("Task queued.");
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };

  const destroy = async (task: ScheduledTask) => {
    try {
      await mutations.destroy.mutateAsync(task.id ?? "");
      toast.success("Task deleted.");
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };

  const columns: Column<ScheduledTask>[] = [
    {
      id: "name",
      header: "Name",
      cell: (task) => (
        <>
          <Link
            to={editPath(task)}
            className="font-medium text-primary hover:underline"
          >
            {task.name}
          </Link>
          {task.last_error ? (
            <div className="mt-1 text-xs text-destructive" title={task.last_error}>
              Last dispatch failed
            </div>
          ) : task.last_run_at ? (
            <div className="mt-1 text-xs text-muted-foreground">
              Last run <LocalTime value={task.last_run_at} format="compact" />
            </div>
          ) : null}
        </>
      ),
    },
    {
      id: "schedule",
      header: "Schedule",
      className: "text-xs text-muted-foreground",
      cell: (task) => (
        <>
          <div className={cn(task.schedule_label === task.cron_expression && "font-mono")}>{task.schedule_label}</div>
          {!task.enabled && <div className="mt-1 text-muted-foreground">Disabled</div>}
        </>
      ),
    },
    {
      id: "delivery",
      header: "Delivery",
      className: "text-xs text-muted-foreground",
      cell: (task) => (
        <>
          <div>{task.delivery_name || task.delivery_channel}</div>
          {task.delivery_name && <div className="mt-1 text-muted-foreground">{task.delivery_channel}</div>}
        </>
      ),
    },
    {
      id: "actions",
      header: <span className="sr-only">Actions</span>,
      cell: (task) => (
        <div className="flex justify-end gap-2">
          <Button variant="secondary" size="sm" disabled={!task.enabled || mutations.run.isPending} onClick={() => void run(task)}>
            Run now
          </Button>
          <Button asChild variant="secondary" size="sm">
            <Link to={editPath(task)}>Edit</Link>
          </Button>
          <ConfirmDialog
            title={`Delete ${task.name}?`}
            confirmLabel="Delete"
            onConfirm={() => destroy(task)}
            trigger={
              <Button variant="secondary" size="sm" className="text-destructive">
                Delete
              </Button>
            }
          />
        </div>
      ),
    },
  ];

  return (
    <>
      <PageHeader
        title="Scheduled Tasks"
        documentTitle="Scheduled"
        subtitle="Schedule single-turn prompts and deliver their results to Slack."
        icon={Clock}
        actions={
          <Button asChild>
            <Link to="/console/scheduled_tasks/new">New Task</Link>
          </Button>
        }
      />
      {tasks.isError ? (
        <ErrorAlert>Could not load scheduled tasks: {errorMessage(tasks.error)}</ErrorAlert>
      ) : (
        <DataTable
          caption="Scheduled tasks"
          columns={columns}
          rows={tasks.data}
          loading={tasks.isLoading}
          rowKey={(task) => task.id ?? ""}
          empty="No scheduled tasks yet."
        />
      )}
    </>
  );
}
