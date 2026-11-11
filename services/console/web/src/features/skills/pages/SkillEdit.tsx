import { useNavigate, useParams } from "react-router";
import { toast } from "sonner";

import { NotFound } from "@/app/shell/NotFound";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { ApiError } from "@/lib/api";

import { useSkill, useSkillMutations } from "../api";
import { SkillForm } from "../sections/SkillForm";

export default function SkillEditPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.skills}>
      <SkillEdit />
    </RequireAccess>
  );
}

function SkillEdit() {
  const { id = "" } = useParams();
  const { data: skill, refetch } = useSkill(id);
  const { update } = useSkillMutations(id);
  const navigate = useNavigate();

  // Readers who cannot edit get the same 404 the API returns on update.
  if (!skill.viewer.can_edit) return <NotFound />;

  return (
    <>
      <PageHeader
        title={`Edit ${skill.name}`}
        subtitle={
          skill.visibility === "shared" ? (
            <span className="text-warning">
              This skill is shared. Saving changes makes them live immediately.
            </span>
          ) : undefined
        }
      />
      <SkillForm
        // Remount with the latest content after a stale-version reload.
        key={skill.lock_version}
        skill={skill}
        canManageEditors={skill.viewer.can_manage}
        cancelTo={`/console/skills/${skill.id}`}
        onSubmit={async (input) => {
          try {
            await update.mutateAsync(input);
          } catch (error) {
            if (error instanceof ApiError && error.status === 409) {
              // Like the legacy redirect back to edit: show the latest content.
              toast.error(error.message);
              await refetch();
            }
            throw error;
          }
          toast.success("Skill saved.");
          navigate(`/console/skills/${skill.id}`);
        }}
      />
    </>
  );
}
