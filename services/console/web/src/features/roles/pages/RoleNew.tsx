import { useNavigate } from "react-router";
import { toast } from "sonner";

import { BackLink } from "@/components/back-link";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";

import { useCreateRole } from "../api";
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
  const create = useCreateRole();

  return (
    <>
      <div className="mb-2">
        <BackLink to="/console/roles">back to roles</BackLink>
      </div>
      <PageHeader title="New Role" />
      <RoleForm
        error={create.error}
        onSubmit={async (data) => {
          const body = await create.mutateAsync(data);
          toast.success("Role created.");
          navigate(`/console/roles/${body.data.id}`);
        }}
      />
    </>
  );
}
