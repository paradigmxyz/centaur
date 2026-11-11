import { useNavigate } from "react-router";
import { toast } from "sonner";

import { BackLink } from "@/components/back-link";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";

import { API_CLIENT_SCOPES, useCreateApiClient } from "../api";
import { stashIssuedSecret } from "../secret-handoff";
import { ApiClientForm } from "../sections/ApiClientForm";

export default function ApiClientNewPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.api_clients}>
      <ApiClientNew />
    </RequireAccess>
  );
}

function ApiClientNew() {
  const navigate = useNavigate();
  const create = useCreateApiClient();

  return (
    <>
      <div className="mb-2">
        <BackLink to="/console/api_clients">back to API clients</BackLink>
      </div>
      <PageHeader title="New API Client" />
      <ApiClientForm
        mode="new"
        defaults={{ name: "", scopes: [...API_CLIENT_SCOPES] }}
        error={create.error}
        cancelTo="/console/api_clients"
        onSubmit={async (input) => {
          const body = await create.mutateAsync(input);
          stashIssuedSecret(body.data.id, body.data.secret);
          create.reset();
          toast.success("API client created.");
          navigate(`/console/api_clients/${body.data.id}`);
        }}
      />
    </>
  );
}
