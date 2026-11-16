import { useQueryClient } from "@tanstack/react-query";
import { useNavigate, useParams } from "react-router";
import { toast } from "sonner";

import { ErrorAlert } from "@/components/alerts";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { errorMessage } from "@/lib/api";

import { updateScheduledTask, useScheduledTask } from "../api";
import { ScheduledTaskForm } from "../sections/ScheduledTaskForm";

export default function ScheduledTaskEditPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.scheduled_tasks}>
      <ScheduledTaskEdit />
    </RequireAccess>
  );
}

function ScheduledTaskEdit() {
  const { id = "" } = useParams();
  const { data: task, error, isError } = useScheduledTask(id);
  const queryClient = useQueryClient();
  const navigate = useNavigate();

  return (
    <>
      <PageHeader
        title="Edit Scheduled Task"
        documentTitle={`Edit ${task.name ?? ""}`}
        subtitle={task.name}
      />
      {isError && <ErrorAlert>Could not load this task: {errorMessage(error)}</ErrorAlert>}
      <ScheduledTaskForm
        key={task.id}
        task={task}
        submitLabel="Save task"
        onSubmit={async (data) => {
          await updateScheduledTask(queryClient, id, data);
          toast.success("Task saved.");
          navigate("/console/scheduled_tasks");
        }}
      />
    </>
  );
}
