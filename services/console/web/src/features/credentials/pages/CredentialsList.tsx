import { Link } from "react-router";

import { ErrorAlert } from "@/components/alerts";
import { StatusBadge, credentialStatusTone } from "@/components/badges";
import { ControlTabs } from "@/components/control-tabs";
import { DataTable, type Column } from "@/components/data-table";
import { LocalTime } from "@/components/local-time";
import { PageHeader } from "@/components/page-header";
import { usePageParam } from "@/components/pagination";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";
import { pluralize, truncateMiddle } from "@/lib/format";

import { useCredentialList, type BrokerCredential } from "../api";
import { providerLabel } from "../sections/provider";

const linkClass = "text-primary hover:underline";

const columns: Column<BrokerCredential>[] = [
  {
    id: "id",
    header: "ID",
    className: "font-mono text-[13px] whitespace-nowrap",
    cell: (credential) =>
      credential.foreign_id ? (
        <>
          <Link to={`/console/credentials/${credential.id}`} title={credential.foreign_id} className={linkClass}>
            {truncateMiddle(credential.foreign_id)}
          </Link>
          <div className="font-mono text-xs text-muted-foreground">{credential.id}</div>
        </>
      ) : (
        <Link to={`/console/credentials/${credential.id}`} className={linkClass}>
          {credential.id}
        </Link>
      ),
  },
  {
    id: "name",
    header: "Name",
    className: "whitespace-nowrap text-foreground",
    cell: (credential) => (
      <>
        {credential.name ? (
          <span title={credential.name}>{truncateMiddle(credential.name)}</span>
        ) : (
          <span className="text-muted-foreground">unnamed</span>
        )}
        {credential.oauth_app && (
          <span className="ml-1 text-xs text-muted-foreground">(via {providerLabel(credential.oauth_app.provider)} OAuth)</span>
        )}
      </>
    ),
  },
  {
    id: "status",
    header: "Status",
    className: "whitespace-nowrap",
    cell: (credential) => (
      <>
        <StatusBadge tone={credentialStatusTone(credential.status)}>{credential.status}</StatusBadge>
        {credential.status === "dead" && credential.dead_reason && (
          <span className="ml-1 text-xs text-muted-foreground">{credential.dead_reason}</span>
        )}
      </>
    ),
  },
  {
    id: "last_refresh",
    header: "Last Refresh",
    className: "whitespace-nowrap text-muted-foreground",
    cell: (credential) => <LocalTime value={credential.last_refresh} format="relative" />,
  },
];

export default function CredentialsListPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.credentials}>
      <CredentialsList />
    </RequireAccess>
  );
}

function CredentialsList() {
  const [page, setPage] = usePageParam();
  const list = useCredentialList(page);
  const total = list.data?.meta.total;

  return (
    <>
      <ControlTabs />
      <PageHeader
        title="Managed Credentials"
        documentTitle="Credentials"
        subtitle={total === undefined ? " " : `${pluralize(total, "broker credential")}. Token material is never shown.`}
        actions={
          <Button asChild>
            <Link to="/console/credentials/new">+ Add Credential</Link>
          </Button>
        }
      />
      {/* A failed load (or refetch) is an error, never "No managed credentials.";
          rows from an earlier successful load stay visible below the alert. */}
      {list.isError && <ErrorAlert>Could not load credentials: {errorMessage(list.error)}</ErrorAlert>}
      {(!list.isError || (list.data?.data.length ?? 0) > 0) && (
        <DataTable
          caption="Managed credentials"
          columns={columns}
          rows={list.data?.data}
          loading={list.isLoading}
          rowKey={(credential) => credential.id}
          empty="No managed credentials."
          pagination={
            list.data && list.data.meta.total_pages > 1
              ? { meta: list.data.meta, unit: "credential", onPageChange: setPage }
              : undefined
          }
        />
      )}
    </>
  );
}
