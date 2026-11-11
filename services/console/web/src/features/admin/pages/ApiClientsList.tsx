import { Link } from "react-router";

import { ControlTabs } from "@/components/control-tabs";
import { DataTable, type Column } from "@/components/data-table";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";

import { useApiClients, type ApiClient } from "../api";
import { ScopeChips } from "../sections/ScopeChips";

const columns: Column<ApiClient>[] = [
  {
    id: "id",
    header: "Client ID",
    className: "whitespace-nowrap",
    cell: (client) => (
      <Link
        to={`/console/api_clients/${client.id}`}
        className="text-centaur-400 hover:text-centaur-300 hover:underline light:text-primary"
      >
        {client.id}
      </Link>
    ),
  },
  { id: "name", header: "Name", className: "whitespace-nowrap text-heading", cell: (client) => client.name },
  { id: "scopes", header: "Scopes", cell: (client) => <ScopeChips scopes={client.scopes} /> },
  { id: "principals", header: "Principals", className: "text-zinc-400", cell: (client) => client.principals_count },
  {
    id: "status",
    header: "Status",
    className: "text-zinc-400",
    cell: (client) => (client.disabled ? "disabled" : "active"),
  },
];

export default function ApiClientsListPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.api_clients}>
      <ApiClientsList />
    </RequireAccess>
  );
}

function ApiClientsList() {
  const clients = useApiClients();
  return (
    <>
      <ControlTabs />
      <PageHeader
        title="API Clients"
        subtitle="Machine callers of the session API, such as eval runners. Each runs sessions as one of its attached principals."
        actions={
          <Button asChild>
            <Link to="/console/api_clients/new">Add API Client</Link>
          </Button>
        }
      />
      <DataTable
        caption="API clients"
        columns={columns}
        rows={clients.data}
        loading={clients.isLoading}
        rowKey={(client) => client.id}
        empty={clients.isError ? errorMessage(clients.error) : "No API clients yet."}
      />
    </>
  );
}
