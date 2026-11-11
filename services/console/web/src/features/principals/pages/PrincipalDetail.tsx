import { useNavigate, useParams } from "react-router";
import { toast } from "sonner";

import { LabelChips } from "@/components/badges";
import { BackLink } from "@/components/back-link";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { useDocumentTitle } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";
import { displayLabel } from "@/lib/format";

import { usePrincipal, usePrincipalMutations } from "../api";
import { DirectGrantsSection } from "../sections/DirectGrantsSection";
import { EffectiveGrantsSection } from "../sections/EffectiveGrantsSection";
import { RolesSection } from "../sections/RolesSection";
import { SandboxAccessSection } from "../sections/SandboxAccessSection";
import { SlackPermissionsSection } from "../sections/SlackPermissionsSection";

export default function PrincipalDetailPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.principals}>
      <PrincipalDetail />
    </RequireAccess>
  );
}

function PrincipalDetail() {
  const { id = "" } = useParams();
  const { data: principal } = usePrincipal(id);
  const mutations = usePrincipalMutations(id);
  const navigate = useNavigate();
  useDocumentTitle(displayLabel(principal));

  const onDelete = async () => {
    try {
      await mutations.destroy.mutateAsync();
      toast.success(`Deleted principal ${displayLabel(principal)}.`);
      navigate("/console/principals");
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };

  return (
    <>
      <div className="mb-8">
        <BackLink to="/console/principals">back to principals</BackLink>
        <div className="mt-2 flex flex-wrap items-center gap-3">
          <h1 className="text-2xl font-semibold tracking-tight">{principal.name || principal.foreign_id || "Principal"}</h1>
          <div className="ml-auto">
            <ConfirmDialog
              title="Delete this principal?"
              description="Direct grants, role assignments, MCP tokens, and sync snapshots are removed. Proxies are unassigned. This cannot be undone."
              confirmLabel="Delete"
              onConfirm={onDelete}
              trigger={
                <Button variant="destructive" size="sm">
                  Delete
                </Button>
              }
            />
          </div>
        </div>
        <div className="mt-2 flex flex-wrap items-center gap-2 text-sm">
          <span className="rounded-md border bg-muted/50 px-1.5 py-0.5 font-mono text-xs text-foreground">{principal.id}</span>
          {principal.foreign_id && (
            <>
              <span className="text-muted-foreground">·</span>
              <span className="text-muted-foreground">
                foreign_id: <span className="font-mono text-xs text-foreground">{principal.foreign_id}</span>
              </span>
            </>
          )}
        </div>
        <div className="mt-3">
          <LabelChips labels={principal.labels} />
        </div>
      </div>

      <SlackPermissionsSection principal={principal} mutations={mutations} />
      <SandboxAccessSection principal={principal} mutations={mutations} />
      <RolesSection principal={principal} mutations={mutations} />
      <DirectGrantsSection principal={principal} mutations={mutations} />
      <EffectiveGrantsSection principal={principal} />
    </>
  );
}
