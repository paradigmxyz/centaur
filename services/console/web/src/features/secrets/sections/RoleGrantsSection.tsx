import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { toast } from "sonner";

import { AppLink } from "@/components/app-link";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { DataTable, type Column } from "@/components/data-table";
import { EmptyState } from "@/components/empty-state";
import { SectionHeading } from "@/components/page-header";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { errorMessage } from "@/lib/api";
import { displayLabel } from "@/lib/format";
import { useSubmit } from "@/lib/use-submit";

import { assignSecretRole, unassignSecretRole, useRoleGrants, type RoleGrant } from "../api";
import { QueryErrorAlert } from "../query-error";

export function RoleGrantsSection({ kind, id }: { kind: string; id: string }) {
  const grants = useRoleGrants(kind, id);
  const queryClient = useQueryClient();
  const assignRole = useSubmit((roleId: string) => assignSecretRole(queryClient, kind, id, roleId));
  const [selected, setSelected] = useState("");
  const assignable = grants.data?.assignable_roles ?? [];
  const rows = grants.data?.data ?? [];

  const assign = async () => {
    const role = assignable.find((candidate) => candidate.id === selected) ?? assignable[0];
    if (!role) return;
    try {
      await assignRole.run(role.id);
      setSelected("");
      toast.success(`Assigned secret to ${displayLabel(role)}.`);
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };

  const columns: Column<RoleGrant>[] = [
    {
      id: "role",
      header: "Role",
      className: "whitespace-nowrap text-foreground",
      cell: (grant) => grant.role.name || <span className="text-muted-foreground">unnamed</span>,
    },
    {
      id: "id",
      header: "ID",
      className: "font-mono text-xs whitespace-nowrap",
      cell: (grant) => (
        <AppLink
          to={`/console/roles/${grant.role.id}`}
          className="text-primary hover:underline"
        >
          {grant.role.id}
        </AppLink>
      ),
    },
    { id: "foreign_id", header: "Foreign ID", className: "font-mono text-xs whitespace-nowrap", cell: (grant) => grant.role.foreign_id || "—" },
    {
      id: "actions",
      header: <span className="sr-only">Actions</span>,
      className: "whitespace-nowrap text-right",
      cell: (grant) => (
        <ConfirmDialog
          title={`Unassign this secret from ${displayLabel(grant.role)}?`}
          confirmLabel="Unassign"
          onConfirm={async () => {
            try {
              await unassignSecretRole(queryClient, kind, id, grant.id);
              toast.success("Unassigned secret from role.");
            } catch (error) {
              toast.error(errorMessage(error));
            }
          }}
          trigger={
            <Button variant="secondary" size="sm" className="hover:border-destructive/40 hover:text-destructive">
              Unassign
            </Button>
          }
        />
      ),
    },
  ];

  return (
    <section id="roles" className="mb-8">
      <div className="mb-4 flex flex-wrap items-end justify-between gap-3">
        <SectionHeading>Roles</SectionHeading>
        {assignable.length > 0 && (
          <div className="flex flex-wrap items-center gap-3">
            <Select value={selected || assignable[0]?.id} onValueChange={setSelected}>
              <SelectTrigger className="w-72 max-w-full" aria-label="Role to assign">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {assignable.map((role) => (
                  <SelectItem key={role.id} value={role.id}>
                    {displayLabel(role)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Button onClick={() => void assign()} disabled={assignRole.pending}>
              Assign role
            </Button>
          </div>
        )}
      </div>
      {grants.isError && (
        <QueryErrorAlert what="role grants" error={grants.error} onRetry={() => void grants.refetch()} />
      )}
      {grants.isLoading ? (
        <p className="text-sm text-muted-foreground">Loading…</p>
      ) : rows.length > 0 ? (
        // Stale non-empty rows stay visible after a failed refetch.
        <DataTable caption="Roles" columns={columns} rows={rows} rowKey={(grant) => grant.id} empty="No roles assigned." />
      ) : (
        // The empty state only for a successful empty response.
        grants.isSuccess && <EmptyState>No roles assigned.</EmptyState>
      )}
    </section>
  );
}
