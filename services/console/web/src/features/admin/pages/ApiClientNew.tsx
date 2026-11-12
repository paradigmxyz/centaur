import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router";
import { toast } from "sonner";

import { BackLink } from "@/components/back-link";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";

import { API_CLIENT_SCOPES, createApiClient } from "../api";
import { apiClientPath, useStashIssuedSecret } from "../secret-handoff";
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
  const queryClient = useQueryClient();
  const stashIssuedSecret = useStashIssuedSecret();

  return (
    <>
      <div className="mb-2">
        <BackLink to="/console/api_clients">back to API clients</BackLink>
      </div>
      <PageHeader title="New API Client" />
      <ApiClientForm
        mode="new"
        defaults={{ name: "", scopes: [...API_CLIENT_SCOPES] }}
        cancelTo="/console/api_clients"
        onSubmit={async (input) => {
          // The response carries the plaintext secret: hand it straight to the
          // detail page (route-scoped state), never to a query cache.
          const body = await createApiClient(queryClient, input);
          const id = body.data.id;
          stashIssuedSecret(id, body.data.secret);
          toast.success("API client created.");
          navigate(apiClientPath(id));
        }}
      />
    </>
  );
}
