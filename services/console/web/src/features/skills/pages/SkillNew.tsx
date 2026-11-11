import { useNavigate } from "react-router";
import { toast } from "sonner";

import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";

import { useCreateSkill } from "../api";
import { SkillForm } from "../sections/SkillForm";

export default function SkillNewPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.skills}>
      <SkillNew />
    </RequireAccess>
  );
}

function SkillNew() {
  const navigate = useNavigate();
  const create = useCreateSkill();

  return (
    <>
      <PageHeader title="New Skill" subtitle="Create a private skill. You can share it after it validates." />
      <SkillForm
        canManageEditors
        cancelTo="/console/skills"
        onSubmit={async (input) => {
          const body = await create.mutateAsync(input);
          toast.success("Skill created.");
          navigate(`/console/skills/${body.data.id}`);
        }}
      />
    </>
  );
}
