import { useNavigate, useParams } from "react-router";
import { toast } from "sonner";

import { BackLink } from "@/components/back-link";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { displayLabel } from "@/lib/format";

import { useRole, useRoleMutations } from "../api";
import { RoleForm } from "../sections/RoleForm";

export default function RoleEditPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.roles}>
      <RoleEdit />
    </RequireAccess>
  );
}

function RoleEdit() {
  const { id = "" } = useParams();
  const { data: role } = useRole(id);
  const { update } = useRoleMutations(id);
  const navigate = useNavigate();

  return (
    <>
      <div className="mb-2">
        <BackLink to={`/console/roles/${role.id}`}>back to role</BackLink>
      </div>
      <PageHeader title="Edit Role" documentTitle={`Edit ${displayLabel(role)}`} />
      <RoleForm
        role={role}
        error={update.error}
        onSubmit={async (data) => {
          await update.mutateAsync(data);
          toast.success("Role updated.");
          navigate(`/console/roles/${role.id}`);
        }}
      />
    </>
  );
}
