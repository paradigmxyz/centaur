import { Link, useParams } from "react-router";

import { LabelChips } from "@/components/badges";
import { BackLink } from "@/components/back-link";
import { useDocumentTitle } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { displayLabel } from "@/lib/format";

import { useRole, useRoleMutations } from "../api";
import { RoleGrantsSection } from "../sections/RoleGrantsSection";
import { RoleSlackPermissionsSection } from "../sections/RoleSlackPermissionsSection";

export default function RoleDetailPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.roles}>
      <RoleDetail />
    </RequireAccess>
  );
}

function RoleDetail() {
  const { id = "" } = useParams();
  const { data: role } = useRole(id);
  const mutations = useRoleMutations(id);
  const title = displayLabel(role);
  useDocumentTitle(title);

  return (
    <>
      <div className="mb-8">
        <BackLink to="/console/roles">back to roles</BackLink>
        <div className="mt-2 flex flex-wrap items-center gap-3">
          <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
          <div className="ml-auto flex items-center gap-2">
            <Button asChild variant="secondary" size="sm">
              <Link to={`/console/roles/${role.id}/edit`}>Edit</Link>
            </Button>
          </div>
        </div>
        <div className="mt-2 flex flex-wrap items-center gap-2 text-sm">
          <span className="rounded-md border bg-muted/50 px-1.5 py-0.5 font-mono text-xs text-foreground">{role.id}</span>
          {role.foreign_id && (
            <>
              <span className="text-muted-foreground">·</span>
              <span className="text-muted-foreground">
                foreign_id: <span className="break-all font-mono text-xs text-foreground">{role.foreign_id}</span>
              </span>
            </>
          )}
        </div>
        <div className="mt-3">
          <LabelChips labels={role.labels} />
        </div>
      </div>

      <RoleSlackPermissionsSection role={role} mutations={mutations} />
      <RoleGrantsSection role={role} mutations={mutations} />
    </>
  );
}
