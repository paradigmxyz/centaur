import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router";
import { toast } from "sonner";

import { ErrorAlert } from "@/components/alerts";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { errorMessage } from "@/lib/api";

import { createScheduledTask, useNewScheduledTask } from "../api";
import { ScheduledTaskForm } from "../sections/ScheduledTaskForm";

export default function ScheduledTaskNewPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.scheduled_tasks}>
      <ScheduledTaskNew />
    </RequireAccess>
  );
}

function ScheduledTaskNew() {
  const { data: defaults, error, isError } = useNewScheduledTask();
  const queryClient = useQueryClient();
  const navigate = useNavigate();

  return (
    <>
      <PageHeader
        title="New Scheduled Task"
        subtitle="Schedule one agent prompt and deliver its response to Slack."
      />
      {isError && <ErrorAlert>Could not load this task: {errorMessage(error)}</ErrorAlert>}
      <ScheduledTaskForm
        task={defaults}
        submitLabel="Create task"
        onSubmit={async (data) => {
          await createScheduledTask(queryClient, data);
          toast.success("Task created.");
          navigate("/console/scheduled_tasks");
        }}
      />
    </>
  );
}
