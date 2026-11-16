import { ErrorAlert } from "@/components/alerts";
import { AppLink } from "@/components/app-link";
import { ResourceBadge } from "@/components/badges";
import { DataTable, type Column } from "@/components/data-table";
import { EmptyState } from "@/components/empty-state";
import { SectionHeading } from "@/components/page-header";
import { usePageParam } from "@/components/pagination";
import { errorMessage } from "@/lib/api";
import { displayLabel } from "@/lib/format";
import { secretKindLabel } from "@/lib/secret-kinds";

import { useEffectiveGrants, type EffectiveGrant, type Principal } from "../api";
import { secretPath } from "./DirectGrantsSection";

const columns: Column<EffectiveGrant>[] = [
  {
    id: "type",
    header: "Type",
    className: "w-40 whitespace-nowrap",
    cell: (row) => <ResourceBadge>{secretKindLabel(row.kind)}</ResourceBadge>,
  },
  {
    id: "id",
    header: "ID",
    className: "font-mono text-xs w-32 whitespace-nowrap",
    cell: (row) => (
      <AppLink to={secretPath(row)} className="text-primary hover:underline">
        {row.id}
      </AppLink>
    ),
  },
  {
    id: "foreign_id",
    header: "Foreign ID",
    className: "font-mono text-xs w-[28%] whitespace-nowrap",
    cell: (row) =>
      row.foreign_id ? (
        <span className="block truncate" title={row.foreign_id}>
          {row.foreign_id}
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
      row.name ? (
        <span className="block truncate" title={row.name}>
          {row.name}
        </span>
      ) : (
        <span className="text-muted-foreground">unnamed</span>
      ),
  },
  {
    id: "source",
    header: "Source",
    className: "w-56",
    cell: (row) => (
      <div className="flex flex-wrap gap-1">
        {row.direct && (
          <span className="inline-flex items-center rounded-md bg-primary/10 px-2 py-0.5 text-[11px] text-primary ring-1 ring-primary/30">
            direct
          </span>
        )}
        {row.roles.map((role) => (
          <span
            key={role.id}
            title={`${displayLabel(role)} (${role.id})`}
            className="inline-flex max-w-full min-w-0 items-center rounded-md bg-secondary px-2 py-0.5 text-[11px] text-foreground ring-1 ring-border"
          >
            <span className="truncate">via {displayLabel(role)}</span>
          </span>
        ))}
      </div>
    ),
  },
];

export function EffectiveGrantsSection({ principal }: { principal: Principal }) {
  const [page, setPage] = usePageParam("effective_grants_page");
  const grants = useEffectiveGrants(principal.id, page);

  return (
    <section id="effective-grants">
      <SectionHeading>Effective Grants</SectionHeading>
      <p className="mb-4 text-sm text-muted-foreground">
        Every secret this principal resolves to: the direct grants above plus grants inherited from its roles. Read-only.
      </p>
      {grants.isError ? (
        <ErrorAlert>Could not load effective grants: {errorMessage(grants.error)}</ErrorAlert>
      ) : grants.data && grants.data.meta.total === 0 ? (
        <EmptyState className="py-10 text-center">This principal has no effective grants.</EmptyState>
      ) : (
        <DataTable
          fixed
          caption="Effective grants"
          columns={columns}
          rows={grants.data?.data}
          loading={grants.isLoading}
          rowKey={(row) => `${row.kind}:${row.id}`}
          empty="This principal has no effective grants."
          pagination={grants.data ? { meta: grants.data.meta, unit: "effective grant", onPageChange: setPage } : undefined}
        />
      )}
    </section>
  );
}
