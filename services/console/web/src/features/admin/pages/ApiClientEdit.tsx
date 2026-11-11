import { useNavigate, useParams } from "react-router";
import { toast } from "sonner";

import { BackLink } from "@/components/back-link";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";

import { useApiClient, useUpdateApiClient } from "../api";
import { ApiClientForm } from "../sections/ApiClientForm";

export default function ApiClientEditPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.api_clients}>
      <ApiClientEdit />
    </RequireAccess>
  );
}

function ApiClientEdit() {
  const { id = "" } = useParams();
  const { data: client } = useApiClient(id);
  const update = useUpdateApiClient(id);
  const navigate = useNavigate();
  const detailPath = `/console/api_clients/${client.id}`;

  return (
    <>
      <div className="mb-2">
        <BackLink to={detailPath}>back to API client</BackLink>
      </div>
      <PageHeader title={`Edit ${client.name}`} />
      <ApiClientForm
        mode="edit"
        defaults={{ name: client.name, scopes: client.scopes }}
        cancelTo={detailPath}
        onSubmit={async (input) => {
          await update.mutateAsync(input);
          toast.success("API client updated.");
          navigate(detailPath);
        }}
      />
    </>
  );
}
