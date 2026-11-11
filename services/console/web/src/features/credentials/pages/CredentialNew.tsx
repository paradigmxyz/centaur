import { useNavigate } from "react-router";
import { toast } from "sonner";

import { ResourceBadge } from "@/components/badges";
import { BackLink } from "@/components/back-link";
import { useDocumentTitle } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";

import { useCreateCredential } from "../api";
import { CredentialForm } from "../sections/CredentialForm";

export default function CredentialNewPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.credentials}>
      <CredentialNew />
    </RequireAccess>
  );
}

function CredentialNew() {
  const navigate = useNavigate();
  const create = useCreateCredential();
  useDocumentTitle("New credential");

  return (
    <>
      <div className="mb-8">
        <BackLink to="/console/credentials">back to credentials</BackLink>
        <div className="mt-2 flex flex-wrap items-center gap-3">
          <h1 className="text-2xl font-bold text-heading">New credential</h1>
          <ResourceBadge>Broker</ResourceBadge>
        </div>
      </div>
      <CredentialForm
        submitLabel="Create credential"
        error={create.error}
        onSubmit={async (payload) => {
          const body = await create.mutateAsync(payload);
          toast.success("Credential created.");
          navigate(`/console/credentials/${body.data.id}`);
        }}
      />
    </>
  );
}
