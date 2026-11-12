import { useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { toast } from "sonner";

import { ErrorAlert } from "@/components/alerts";
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
import { useSubmit } from "@/lib/use-submit";

import { grantRoleSecret, revokeRoleGrant, useAssignableSecretCount, useRoleGrants, type RoleGrant } from "../api";
import type { SectionProps } from "./types";

function secretPath(secret: Pick<GrantableSummary, "kind" | "id">) {
  return `/console/secrets/${secret.kind}/${secret.id}`;
}

/** The role's secret grants (legacy "Secrets" section): grant picker + table. */
export function RoleGrantsSection({ role }: SectionProps) {
  const queryClient = useQueryClient();
  const grantSecret = useSubmit((secret: GrantableSummary) => grantRoleSecret(queryClient, role.id, secret));
  const [page, setPage] = usePageParam("grants_page");
  const grants = useRoleGrants(role.id, page);
  const [selected, setSelected] = useState<GrantableSummary | null>(null);
  // The server leaves secrets already granted to the role out of the picker;
  // like the legacy page, the form is hidden once nothing is left to grant.
  const assignable = useAssignableSecretCount(role.id);
  const showGrantForm = assignable.isError || (assignable.data ?? 0) > 0;

  const grant = async () => {
    if (!selected) {
      toast.error("Pick a secret to grant.");
      return;
    }
    try {
      await grantSecret.run(selected);
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
        className: "font-mono text-xs w-32 whitespace-nowrap",
        cell: (row) => (
          <AppLink to={secretPath(row.grantable)} className="text-primary hover:underline">
            {row.grantable.id}
          </AppLink>
        ),
      },
      {
        id: "foreign_id",
        header: "Foreign ID",
        className: "font-mono text-xs w-[28%] whitespace-nowrap",
        cell: (row) =>
          row.grantable.foreign_id ? (
            <span className="block truncate" title={row.grantable.foreign_id}>
              {row.grantable.foreign_id}
            </span>
          ) : (
            <span className="text-muted-foreground">—</span>
          ),
      },
      {
        id: "name",
        header: "Name",
        className: "whitespace-nowrap text-foreground",
        cell: (row) =>
          row.grantable.name ? (
            <span className="block truncate" title={row.grantable.name}>
              {row.grantable.name}
            </span>
          ) : (
            <span className="text-muted-foreground">unnamed</span>
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
                await revokeRoleGrant(queryClient, role.id, row.id);
                toast.success("Revoked grant.");
              } catch (error) {
                toast.error(errorMessage(error));
              }
            }}
            trigger={
              <Button variant="secondary" size="sm" className="hover:border-destructive/40 hover:text-destructive">
                Revoke
              </Button>
            }
          />
        ),
      },
    ],
    [queryClient, role.id],
  );

  return (
    <section id="secrets">
      <div className="mb-3 flex flex-wrap items-center justify-between gap-3">
        <SectionHeading className="mb-0">Secrets</SectionHeading>
        {showGrantForm && (
          <div className="flex w-full items-center gap-3 sm:w-auto">
            <div className="min-w-0 flex-1 sm:w-80 sm:flex-none">
              <SecretGrantPicker value={selected} onChange={setSelected} excludeGrantee={`role:${role.id}`} />
            </div>
            <Button onClick={() => void grant()} disabled={grantSecret.pending}>
              Grant secret
            </Button>
          </div>
        )}
      </div>
      {grants.isError ? (
        <ErrorAlert>Could not load the role's secret grants: {errorMessage(grants.error)}</ErrorAlert>
      ) : grants.data && grants.data.meta.total === 0 ? (
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
