import { useQuery } from "@tanstack/react-query";
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
import { api, errorMessage, type ListEnvelope } from "@/lib/api";
import { secretOptionLabel, type GrantableSummary } from "@/lib/secret-kinds";

import { principalKeys, useDirectGrants, type DirectGrant } from "../api";
import type { SectionProps } from "./types";

export function secretPath(secret: Pick<GrantableSummary, "kind" | "id">) {
  return `/console/secrets/${secret.kind}/${secret.id}`;
}


export function DirectGrantsSection({ principal, mutations }: SectionProps) {
  const [page, setPage] = usePageParam("direct_grants_page");
  const grants = useDirectGrants(principal.id, page);
  const [selected, setSelected] = useState<GrantableSummary | null>(null);

  // Secrets already granted directly are left out of the picker; role-inherited
  // ones stay offered so they can be promoted to a direct grant.
  const granted = useQuery({
    queryKey: principalKeys.sub(principal.id, "grants", { limit: 200 }),
    queryFn: ({ signal }) =>
      api.get<ListEnvelope<DirectGrant>>(`/api/v1/principals/${principal.id}/grants`, { limit: 200 }, signal),
    select: (body) => new Set(body.data.map((grant) => grant.grantable.id)),
  });

  const grant = async () => {
    if (!selected) {
      toast.error("Pick a secret to grant.");
      return;
    }
    try {
      await mutations.grantSecret.mutateAsync(selected);
      toast.success(`Granted ${secretOptionLabel(selected).replace(/ \(.*\)$/, "")}.`);
      setSelected(null);
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };

  const columns = useMemo<Column<DirectGrant>[]>(
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
    <section id="direct-grants" className="mb-8">
      <SectionHeading>Direct Grants</SectionHeading>
      <p className="mb-4 text-sm text-muted-foreground">
        Secrets granted to this principal directly. Grants inherited from roles are managed on the role, not here.
      </p>
      {grants.data && grants.data.meta.total === 0 ? (
        <EmptyState className="mb-4 py-10 text-center">No direct grants. Use the form below to grant a secret.</EmptyState>
      ) : (
        <DataTable
          fixed
          className="mb-4"
          caption="Direct grants"
          columns={columns}
          rows={grants.data?.data}
          loading={grants.isLoading}
          rowKey={(row) => row.id}
          empty="No direct grants."
          pagination={grants.data ? { meta: grants.data.meta, unit: "direct grant", onPageChange: setPage } : undefined}
        />
      )}
      <div className="flex flex-wrap items-center gap-3">
        <SecretGrantPicker value={selected} onChange={setSelected} excludeIds={granted.data} />
        <Button onClick={() => void grant()} disabled={mutations.grantSecret.isPending}>
          Grant secret
        </Button>
      </div>
    </section>
  );
}
