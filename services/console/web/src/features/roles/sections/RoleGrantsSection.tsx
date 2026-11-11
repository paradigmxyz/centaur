import { useMemo, useState } from "react";
import { toast } from "sonner";

import { AppLink } from "@/components/app-link";
import { ResourceBadge } from "@/components/badges";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { DataTable, type Column } from "@/components/data-table";
import { EmptyState } from "@/components/empty-state";
import { SectionHeading } from "@/components/page-header";
import { usePageParam } from "@/components/pagination";
import { SecretGrantPicker } from "@/components/secret-grant-picker";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";
import { displayLabel } from "@/lib/format";
import type { GrantableSummary } from "@/lib/secret-kinds";

import { useRoleGrants, type RoleGrant } from "../api";
import type { SectionProps } from "./types";

function secretPath(secret: Pick<GrantableSummary, "kind" | "id">) {
  return `/console/secrets/${secret.kind}/${secret.id}`;
}

/** The role's secret grants (legacy "Secrets" section): grant picker + table. */
export function RoleGrantsSection({ role, mutations }: SectionProps) {
  const [page, setPage] = usePageParam("grants_page");
  const grants = useRoleGrants(role.id, page);
  const [selected, setSelected] = useState<GrantableSummary | null>(null);
  // Secrets already granted to the role are left out of the picker.
  const granted = useMemo(() => new Set(grants.data?.data.map((grant) => grant.grantable.id)), [grants.data]);

  const grant = async () => {
    if (!selected) {
      toast.error("Pick a secret to grant.");
      return;
    }
    try {
      await mutations.grantSecret.mutateAsync(selected);
      toast.success(`Granted ${displayLabel(selected)}.`);
      setSelected(null);
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };

  const columns = useMemo<Column<RoleGrant>[]>(
    () => [
      {
        id: "type",
        header: "Type",
        className: "w-40 whitespace-nowrap",
        cell: (row) => <ResourceBadge>{row.grantable.kind_label}</ResourceBadge>,
      },
      {
        id: "id",
        header: "ID",
        className: "w-32 whitespace-nowrap",
        cell: (row) => (
          <AppLink to={secretPath(row.grantable)} className="text-centaur-400 hover:underline light:text-primary">
            {row.grantable.id}
          </AppLink>
        ),
      },
      {
        id: "foreign_id",
        header: "Foreign ID",
        className: "w-[28%] whitespace-nowrap",
        cell: (row) =>
          row.grantable.foreign_id ? (
            <span className="block truncate" title={row.grantable.foreign_id}>
              {row.grantable.foreign_id}
            </span>
          ) : (
            <span className="text-subtle-foreground">—</span>
          ),
      },
      {
        id: "name",
        header: "Name",
        className: "whitespace-nowrap text-heading",
        cell: (row) =>
          row.grantable.name ? (
            <span className="block truncate" title={row.grantable.name}>
              {row.grantable.name}
            </span>
          ) : (
            <span className="text-subtle-foreground">unnamed</span>
          ),
      },
      {
        id: "actions",
        header: <span className="sr-only">Actions</span>,
        className: "w-28 whitespace-nowrap text-right",
        cell: (row) => (
          <ConfirmDialog
            title="Revoke this grant?"
            confirmLabel="Revoke"
            onConfirm={async () => {
              try {
                await mutations.revokeGrant.mutateAsync(row.id);
                toast.success("Revoked grant.");
              } catch (error) {
                toast.error(errorMessage(error));
              }
            }}
            trigger={
              <Button variant="secondary" size="sm" className="hover:border-red-500/40 hover:text-red-300">
                Revoke
              </Button>
            }
          />
        ),
      },
    ],
    [mutations.revokeGrant],
  );

  return (
    <section id="secrets">
      <div className="mb-4 flex flex-wrap items-end justify-between gap-3">
        <SectionHeading>Secrets</SectionHeading>
        <div className="flex flex-wrap items-center gap-3">
          <SecretGrantPicker value={selected} onChange={setSelected} excludeIds={granted} />
          <Button onClick={() => void grant()} disabled={mutations.grantSecret.isPending}>
            Grant secret
          </Button>
        </div>
      </div>
      {grants.data && grants.data.meta.total === 0 ? (
        <EmptyState>No secrets granted.</EmptyState>
      ) : (
        <DataTable
          fixed
          caption="Secret grants"
          columns={columns}
          rows={grants.data?.data}
          loading={grants.isLoading}
          rowKey={(row) => row.id}
          empty="No secrets granted."
          pagination={
            grants.data && grants.data.meta.total_pages > 1
              ? { meta: grants.data.meta, unit: "grant", onPageChange: setPage }
              : undefined
          }
        />
      )}
    </section>
  );
}
