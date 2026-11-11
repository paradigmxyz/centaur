import { useNavigate, useParams } from "react-router";
import { toast } from "sonner";

import { ResourceBadge } from "@/components/badges";
import { BackLink } from "@/components/back-link";
import { useDocumentTitle } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";

import { credentialTitle, useCredential, useUpdateCredential } from "../api";
import { CredentialForm } from "../sections/CredentialForm";

export default function CredentialEditPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.credentials}>
      <CredentialEdit />
    </RequireAccess>
  );
}

function CredentialEdit() {
  const { id = "" } = useParams();
  const { data: credential } = useCredential(id);
  const update = useUpdateCredential(id);
  const navigate = useNavigate();
  const title = credentialTitle(credential);
  useDocumentTitle(`Edit ${title}`);

  return (
    <>
      <div className="mb-8">
        <BackLink to={`/console/credentials/${credential.id}`}>back to credential</BackLink>
        <div className="mt-2 flex flex-wrap items-center gap-3">
          <h1 className="text-2xl font-bold text-heading">Edit {title}</h1>
          <ResourceBadge>Broker</ResourceBadge>
        </div>
      </div>
      <CredentialForm
        credential={credential}
        submitLabel="Save changes"
        error={update.error}
        onSubmit={async (payload) => {
          await update.mutateAsync(payload);
          toast.success("Credential updated.");
          navigate(`/console/credentials/${credential.id}`);
        }}
      />
    </>
  );
}
