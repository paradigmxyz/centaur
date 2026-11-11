import { Link, useSearchParams } from "react-router";

import { ErrorAlert } from "@/components/alerts";
import { RolePill, ResourceBadge } from "@/components/badges";
import { ControlTabs } from "@/components/control-tabs";
import { DataTable, type Column } from "@/components/data-table";
import { LocalTime } from "@/components/local-time";
import { PageHeader } from "@/components/page-header";
import { usePageParam } from "@/components/pagination";
import { RequireAccess } from "@/components/require-access";
import { SearchBox } from "@/components/search-box";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";
import { displayLabel, pluralize, truncateMiddle } from "@/lib/format";
import { cn } from "@/lib/utils";

import { usePrincipalList, usePrincipalTotal, type Principal } from "../api";

const columns: Column<Principal>[] = [
  {
    id: "name",
    header: "Name",
    className: "w-[28%] overflow-hidden text-heading",
    cell: (principal) => {
      const name = principal.name || "unnamed";
      return (
        <Link
          to={`/console/principals/${principal.id}`}
          title={name}
          className={cn(
            "block min-w-0 truncate text-centaur-400 hover:text-centaur-300 hover:underline light:text-primary",
            !principal.name && "text-subtle-foreground",
          )}
        >
          {truncateMiddle(name, 64)}
        </Link>
      );
    },
  },
  {
    id: "kind",
    header: "Kind",
    className: "w-[14%] overflow-hidden whitespace-nowrap",
    cell: (principal) => <ResourceBadge title={principal.kind}>{principal.kind.toUpperCase()}</ResourceBadge>,
  },
  {
    id: "roles",
    header: "Roles",
    className: "w-[32%] overflow-hidden",
    cell: (principal) =>
      principal.roles.length === 0 ? (
        <span className="text-subtle-foreground">—</span>
      ) : (
        <div className="flex min-w-0 flex-wrap gap-1">
          {[...principal.roles]
            .sort((a, b) => displayLabel(a).localeCompare(displayLabel(b)))
            .map((role) => (
              <RolePill key={role.id} title={displayLabel(role)}>
                {truncateMiddle(displayLabel(role), 30)}
              </RolePill>
            ))}
        </div>
      ),
  },
  {
    id: "created",
    header: "Created",
    className: "w-[13%] whitespace-nowrap text-xs text-muted-foreground",
    cell: (principal) => <LocalTime value={principal.created_at} format="compact" />,
  },
  {
    id: "updated",
    header: "Updated",
    className: "w-[13%] whitespace-nowrap text-xs text-muted-foreground",
    cell: (principal) => <LocalTime value={principal.updated_at} format="compact" />,
  },
];

export default function PrincipalsListPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.principals}>
      <PrincipalsList />
    </RequireAccess>
  );
}

function PrincipalsList() {
  const [params] = useSearchParams();
  const q = params.get("q") ?? "";
  const [page, setPage] = usePageParam();
  const list = usePrincipalList({ q, page });
  const total = usePrincipalTotal();

  return (
    <>
      <ControlTabs />
      <PageHeader
        title="Principals"
        subtitle={total.data === undefined ? " " : `${pluralize(total.data, "principal")} total.`}
        actions={
          <Button asChild>
            <Link to="/console/principals/new">Add Principal</Link>
          </Button>
        }
      />
      <SearchBox placeholder="Search names or foreign IDs" label="Search principals" />
      {!list.isError && total.isError && (
        <ErrorAlert>Could not load the principal count: {errorMessage(total.error)}</ErrorAlert>
      )}
      {list.isError ? (
        <ErrorAlert>Could not load principals: {errorMessage(list.error)}</ErrorAlert>
      ) : (
      <DataTable
        fixed
        caption="Principals"
        columns={columns}
        rows={list.data?.data}
        loading={list.isLoading}
        rowKey={(principal) => principal.id}
        empty={q ? `No principals match “${q}”.` : "No principals yet."}
        pagination={list.data ? { meta: list.data.meta, unit: "principal", onPageChange: setPage } : undefined}
      />
      )}
    </>
  );
}
