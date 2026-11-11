import { useMemo, useState } from "react";
import { toast } from "sonner";

import { ErrorAlert } from "@/components/alerts";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { DataTable, type Column } from "@/components/data-table";
import { EmptyState } from "@/components/empty-state";
import { SectionHeading } from "@/components/page-header";
import { usePageParam } from "@/components/pagination";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { errorMessage } from "@/lib/api";
import { displayLabel } from "@/lib/format";

import { useAllRoles, usePrincipalRoles, type RoleSummary } from "../api";
import type { SectionProps } from "./types";

const PER_PAGE = 50;

export function RolesSection({ principal, mutations }: SectionProps) {
  const roles = usePrincipalRoles(principal.id);
  const allRoles = useAllRoles();
  const [page, setPage] = usePageParam("roles_page");
  const [selected, setSelected] = useState("");

  const assigned = useMemo(() => roles.data ?? [], [roles.data]);
  const assignable = useMemo(() => {
    const ids = new Set(assigned.map((role) => role.id));
    return (allRoles.data ?? []).filter((role) => !ids.has(role.id));
  }, [allRoles.data, assigned]);

  const totalPages = Math.max(Math.ceil(assigned.length / PER_PAGE), 1);
  const current = Math.min(page, totalPages);
  const pageRows = assigned.slice((current - 1) * PER_PAGE, current * PER_PAGE);

  const assign = async () => {
    const role = assignable.find((candidate) => candidate.id === selected) ?? assignable[0];
    if (!role) return;
    try {
      await mutations.assignRole.mutateAsync(role.id);
      setSelected("");
      toast.success(`Assigned role ${displayLabel(role)}.`);
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };

  const columns: Column<RoleSummary>[] = [
    {
      id: "role",
      header: "Role",
      className: "whitespace-nowrap text-heading",
      cell: (role) => role.name || <span className="text-subtle-foreground">unnamed</span>,
    },
    { id: "id", header: "ID", className: "whitespace-nowrap text-centaur-400 light:text-primary", cell: (role) => role.id },
    { id: "foreign_id", header: "Foreign ID", className: "whitespace-nowrap", cell: (role) => role.foreign_id || "—" },
    {
      id: "actions",
      header: <span className="sr-only">Actions</span>,
      className: "whitespace-nowrap text-right",
      cell: (role) => (
        <ConfirmDialog
          title={`Unassign ${displayLabel(role)} from this principal?`}
          confirmLabel="Unassign"
          onConfirm={async () => {
            try {
              await mutations.unassignRole.mutateAsync(role.id);
              toast.success(`Unassigned role ${displayLabel(role)}.`);
            } catch (error) {
              toast.error(errorMessage(error));
            }
          }}
          trigger={
            <Button variant="secondary" size="sm" className="hover:border-red-500/40 hover:text-red-300">
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
            <Button onClick={() => void assign()} disabled={mutations.assignRole.isPending}>
              Assign role
            </Button>
          </div>
        )}
      </div>
      {roles.isError ? (
        <ErrorAlert>Could not load assigned roles: {errorMessage(roles.error)}</ErrorAlert>
      ) : roles.isLoading ? (
        <p className="text-sm text-muted-foreground">Loading…</p>
      ) : assigned.length === 0 ? (
        <EmptyState>No roles assigned.</EmptyState>
      ) : (
        <DataTable
          caption="Roles"
          columns={columns}
          rows={pageRows}
          rowKey={(role) => role.id}
          empty="No roles assigned."
          pagination={{
            meta: { page: current, limit: PER_PAGE, total: assigned.length, total_pages: totalPages },
            unit: "role",
            onPageChange: setPage,
          }}
        />
      )}
    </section>
  );
}
