import { Link } from "react-router";

import { ErrorAlert } from "@/components/alerts";
import { LabelChips } from "@/components/badges";
import { ControlTabs } from "@/components/control-tabs";
import { DataTable, type Column } from "@/components/data-table";
import { PageHeader } from "@/components/page-header";
import { usePageParam } from "@/components/pagination";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";
import { pluralize, truncateMiddle } from "@/lib/format";

import { useRoleList, type Role } from "../api";

const linkClass = "text-centaur-400 hover:text-centaur-300 hover:underline light:text-primary";

const columns: Column<Role>[] = [
  {
    id: "id",
    header: "ID",
    className: "whitespace-nowrap",
    cell: (role) =>
      role.foreign_id ? (
        <>
          <Link to={`/console/roles/${role.id}`} title={role.foreign_id} className={linkClass}>
            {truncateMiddle(role.foreign_id)}
          </Link>
          <div className="mt-0.5 text-xs text-subtle-foreground">{role.id}</div>
        </>
      ) : (
        <Link to={`/console/roles/${role.id}`} className={linkClass}>
          {role.id}
        </Link>
      ),
  },
  {
    id: "name",
    header: "Name",
    className: "whitespace-nowrap text-heading",
    cell: (role) =>
      role.name ? (
        <span title={role.name}>{truncateMiddle(role.name)}</span>
      ) : (
        <span className="text-subtle-foreground">unnamed</span>
      ),
  },
  {
    id: "labels",
    header: "Labels",
    cell: (role) =>
      Object.keys(role.labels ?? {}).length > 0 ? (
        <LabelChips labels={role.labels} />
      ) : (
        <span className="text-subtle-foreground">—</span>
      ),
  },
  {
    id: "open",
    header: <span className="sr-only">Open</span>,
    className: "w-12 text-right",
    cell: (role) => (
      <Link
        to={`/console/roles/${role.id}`}
        aria-label={`Open ${role.name || role.foreign_id || role.id}`}
        className="text-subtle-foreground hover:text-centaur-400"
      >
        →
      </Link>
    ),
  },
];

export default function RolesListPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.roles}>
      <RolesList />
    </RequireAccess>
  );
}

function RolesList() {
  const [page, setPage] = usePageParam();
  const list = useRoleList(page);
  const total = list.data?.meta.total;

  return (
    <>
      <ControlTabs />
      <PageHeader
        title="Roles"
        subtitle={total === undefined ? " " : `${pluralize(total, "role")}.`}
        actions={
          <Button asChild>
            <Link to="/console/roles/new">Add Role</Link>
          </Button>
        }
      />
      {list.isError ? (
        <ErrorAlert>Could not load roles: {errorMessage(list.error)}</ErrorAlert>
      ) : (
      <DataTable
        caption="Roles"
        columns={columns}
        rows={list.data?.data}
        loading={list.isLoading}
        rowKey={(role) => role.id}
        empty="No roles yet."
        pagination={
          list.data && list.data.meta.total_pages > 1
            ? { meta: list.data.meta, unit: "role", onPageChange: setPage }
            : undefined
        }
      />
      )}
    </>
  );
}
