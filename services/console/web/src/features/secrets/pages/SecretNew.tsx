import { useParams } from "react-router";

import { ResourceBadge } from "@/components/badges";
import { BackLink } from "@/components/back-link";
import { EmptyState } from "@/components/empty-state";
import { useDocumentTitle } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { secretKindLabel } from "@/lib/secret-kinds";

import { isFormKind } from "../api";
import { SecretForm } from "../forms/SecretForm";

export default function SecretNewPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.secrets}>
      <SecretNew />
    </RequireAccess>
  );
}

function SecretNew() {
  const { kind = "" } = useParams();
  const editable = isFormKind(kind);
  useDocumentTitle(editable ? `New ${secretKindLabel(kind)} secret` : "Secret not found");

  if (!editable) {
    // Only the form kinds can be created here (the legacy route 404'd).
    return (
      <>
        <BackLink to="/console/secrets">back to secrets</BackLink>
        <EmptyState className="mt-4">Secret not found.</EmptyState>
      </>
    );
  }

  return (
    <>
      <div className="mb-8">
        <BackLink to="/console/secrets">back to secrets</BackLink>
        <div className="mt-2 flex flex-wrap items-center gap-3">
          <h1 className="text-2xl font-semibold tracking-tight">New secret</h1>
          <ResourceBadge>{secretKindLabel(kind)}</ResourceBadge>
        </div>
      </div>
      <SecretForm key={kind} kind={kind} />
    </>
  );
}
