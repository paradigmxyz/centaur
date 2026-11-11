import { useNavigate, useParams } from "react-router";
import { toast } from "sonner";

import { useSession } from "@/app/session";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";

import { useScheduledTask, useScheduledTaskMutations } from "../api";
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
  const me = useSession();
  const { data: task } = useScheduledTask(id);
  const { update } = useScheduledTaskMutations();
  const navigate = useNavigate();
  const foreignAuthor = task.author_id !== null && task.author_id !== me.user.id;

  return (
    <>
      <PageHeader
        title="Edit Scheduled Task"
        documentTitle={`Edit ${task.name ?? ""}`}
        subtitle={foreignAuthor && task.author_email ? `${task.name} · ${task.author_email}` : task.name}
      />
      <ScheduledTaskForm
        key={task.id}
        task={task}
        foreignAuthor={foreignAuthor}
        submitLabel="Save task"
        onSubmit={async (data) => {
          await update.mutateAsync({ id, data });
          toast.success("Task saved.");
          navigate("/console/scheduled_tasks");
        }}
      />
    </>
  );
}
