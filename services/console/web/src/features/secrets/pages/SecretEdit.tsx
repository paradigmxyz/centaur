import { useParams } from "react-router";

import { WarningAlert } from "@/components/alerts";
import { ResourceBadge } from "@/components/badges";
import { BackLink } from "@/components/back-link";
import { EmptyState } from "@/components/empty-state";
import { useDocumentTitle } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { secretKindLabel } from "@/lib/secret-kinds";

import { isFormKind, secretTitle, useKindRecord, useSecretDetail, type FormKind } from "../api";
import { SecretForm } from "../forms/SecretForm";

export default function SecretEditPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.secrets}>
      <SecretEditRoute />
    </RequireAccess>
  );
}

function SecretEditRoute() {
  const { kind = "", id = "" } = useParams();
  if (!isFormKind(kind)) {
    return (
      <>
        <BackLink to="/console/secrets">back to secrets</BackLink>
        <EmptyState className="mt-4">Secret not found.</EmptyState>
      </>
    );
  }
  return <SecretEdit kind={kind} id={id} />;
}

function SecretEdit({ kind, id }: { kind: FormKind; id: string }) {
  const { data: detail } = useSecretDetail(kind, id);
  const { data: record } = useKindRecord(kind, id);
  const title = secretTitle(detail);
  useDocumentTitle(`Edit ${title}`);
  const managed = detail.managed_by;

  return (
    <>
      <div className="mb-8">
        <BackLink to={`/console/secrets/${kind}/${id}`}>back to secret</BackLink>
        <div className="mt-2 flex flex-wrap items-center gap-3">
          <h1 className="text-2xl font-bold text-heading">Edit {title}</h1>
          <ResourceBadge>{secretKindLabel(kind)}</ResourceBadge>
        </div>
      </div>
      {managed && (
        <WarningAlert className="mb-6 px-4 py-3">
          <span className="font-semibold">Managed secret.</span> Maintained by the{" "}
          {managed.oauth_app_slug ? `${managed.oauth_app_slug} ` : ""}OAuth integration (broker credential{" "}
          {managed.broker_credential_id}). You can tighten its request rules, but changing or removing the token_broker
          source stops token delivery, and re-consent will not repair it.
        </WarningAlert>
      )}
      <SecretForm key={record.updated_at} kind={kind} record={record} />
    </>
  );
}
