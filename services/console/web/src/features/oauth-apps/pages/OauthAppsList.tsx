import { Link } from "react-router";

import { ErrorAlert } from "@/components/alerts";
import { ControlTabs } from "@/components/control-tabs";
import { DataTable, type Column } from "@/components/data-table";
import { PageHeader } from "@/components/page-header";
import { usePageParam } from "@/components/pagination";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";
import { pluralize, truncateMiddle } from "@/lib/format";

import { useOauthAppList, type OauthApp } from "../api";
import { EnabledBadge, ProviderPill } from "../sections/AppBadges";

const columns: Column<OauthApp>[] = [
  {
    id: "slug",
    header: "Slug",
    className: "whitespace-nowrap",
    cell: (app) => (
      <>
        <Link
          to={`/console/oauth_apps/${app.id}`}
          className="font-mono text-primary hover:underline"
        >
          {app.slug}
        </Link>
        <div className="font-mono text-xs text-muted-foreground">{app.id}</div>
      </>
    ),
  },
  {
    id: "description",
    header: "Description",
    className: "whitespace-nowrap text-muted-foreground",
    cell: (app) =>
      app.description ? (
        <span title={app.description}>{truncateMiddle(app.description)}</span>
      ) : (
        <span className="text-muted-foreground">—</span>
      ),
  },
  { id: "provider", header: "Provider", className: "whitespace-nowrap", cell: (app) => <ProviderPill provider={app.provider} /> },
  {
    id: "client_id",
    header: "Client ID",
    className: "whitespace-nowrap text-muted-foreground",
    cell: (app) => <span title={app.client_id}>{truncateMiddle(app.client_id)}</span>,
  },
  { id: "enabled", header: "Enabled", className: "whitespace-nowrap", cell: (app) => <EnabledBadge enabled={app.enabled} /> },
  {
    id: "credentials",
    header: "Credentials",
    className: "whitespace-nowrap text-muted-foreground",
    cell: (app) => app.credentials_count,
  },
];

export default function OauthAppsListPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.oauth_apps}>
      <OauthAppsList />
    </RequireAccess>
  );
}

function OauthAppsList() {
  const [page, setPage] = usePageParam();
  const list = useOauthAppList(page);
  const total = list.data?.meta.total;

  return (
    <>
      <ControlTabs />
      <PageHeader
        title="Apps"
        subtitle={total === undefined ? " " : `${pluralize(total, "OAuth app")} driving the public consent flow.`}
        actions={
          <Button asChild>
            <Link to="/console/oauth_apps/new">+ Add App</Link>
          </Button>
        }
      />
      {list.isError && <ErrorAlert>{errorMessage(list.error)}</ErrorAlert>}
      {/* A failed request shows the alert, never the empty state; stale non-empty rows stay after a failed refetch. */}
      {(!list.isError || (list.data?.data.length ?? 0) > 0) && (
        <DataTable
          caption="OAuth apps"
          columns={columns}
          rows={list.data?.data}
          loading={list.isLoading}
          rowKey={(app) => app.id}
          empty="No apps."
          pagination={
            list.data
              ? { meta: list.data.meta, unit: "app", onPageChange: setPage, hideSinglePage: true }
              : undefined
          }
        />
      )}
    </>
  );
}
