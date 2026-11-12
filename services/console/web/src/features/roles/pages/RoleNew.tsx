import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router";
import { toast } from "sonner";

import { BackLink } from "@/components/back-link";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { useSubmit } from "@/lib/use-submit";

import { createRole, type RoleFormData } from "../api";
import { RoleForm } from "../sections/RoleForm";

export default function RoleNewPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.roles}>
      <RoleNew />
    </RequireAccess>
  );
}

function RoleNew() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const create = useSubmit((data: RoleFormData) => createRole(queryClient, data));

  return (
    <>
      <div className="mb-2">
        <BackLink to="/console/roles">back to roles</BackLink>
      </div>
      <PageHeader title="New Role" />
      <RoleForm
        error={create.error}
        onSubmit={async (data) => {
          const body = await create.run(data);
          toast.success("Role created.");
          navigate(`/console/roles/${body.data.id}`);
        }}
      />
    </>
  );
}
