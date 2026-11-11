import { Link, useNavigate, useParams } from "react-router";
import { toast } from "sonner";

import { AppLink } from "@/components/app-link";
import { LabelChips, ResourceBadge } from "@/components/badges";
import { BackLink } from "@/components/back-link";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { useDocumentTitle } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";
import { secretKindLabel } from "@/lib/secret-kinds";

import { isFormKind, secretTitle, useDeleteSecret, useSecretDetail, type FormKind, type SecretDetail as Detail } from "../api";
import { ConfigurationSection, RequestRulesSection, SignedHeadersSection, SourcesSection } from "../sections/DetailSections";
import { QueryErrorAlert } from "../query-error";
import { RoleGrantsSection } from "../sections/RoleGrantsSection";

export default function SecretDetailPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.secrets}>
      <SecretDetail />
    </RequireAccess>
  );
}

function SecretDetail() {
  const { kind = "", id = "" } = useParams();
  // Suspense query: an initial failure goes to the route error boundary; a
  // failed background refetch keeps the last data and shows the alert.
  const query = useSecretDetail(kind, id);
  const secret = query.data;
  const title = secretTitle(secret);
  useDocumentTitle(title);

  return (
    <>
      {query.isError && <QueryErrorAlert what="this secret" error={query.error} onRetry={() => void query.refetch()} />}
      <div className="mb-8">
        <BackLink to="/console/secrets">back to secrets</BackLink>
        <div className="mt-2 flex flex-wrap items-center gap-3">
          <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
          <ResourceBadge>{secretKindLabel(secret.kind)}</ResourceBadge>
          {secret.editable && isFormKind(secret.kind) && <EditActions kind={secret.kind} secret={secret} />}
        </div>
        <div className="mt-2 flex flex-wrap items-center gap-2 text-sm">
          <span className="rounded-md border bg-muted/50 px-1.5 py-0.5 font-mono text-xs text-foreground">{secret.id}</span>
          {secret.foreign_id && (
            <>
              <span className="text-muted-foreground">·</span>
              <span className="text-muted-foreground">
                foreign_id: <span className="break-all font-mono text-xs text-foreground">{secret.foreign_id}</span>
              </span>
            </>
          )}
        </div>
        {secret.description && <p className="mt-3 max-w-2xl text-sm text-muted-foreground">{secret.description}</p>}
        <div className="mt-3">
          <LabelChips labels={secret.labels} />
        </div>
      </div>

      {secret.managed_by && (
        <div className="mb-8 rounded-xl border border-primary/25 bg-primary/5 px-4 py-3 text-sm text-foreground">
          <span className="font-semibold text-primary">Managed secret.</span> Auto-created by the{" "}
          {secret.managed_by.oauth_app_id ? (
            <AppLink
              to={`/console/oauth_apps/${secret.managed_by.oauth_app_id}`}
              className="text-primary hover:underline"
            >
              {secret.managed_by.oauth_app_slug} OAuth integration
            </AppLink>
          ) : (
            "OAuth consent flow"
          )}{" "}
          to deliver broker credential{" "}
          <AppLink
            to={`/console/credentials/${secret.managed_by.broker_credential_id}`}
            className="text-primary hover:underline"
          >
            {secret.managed_by.broker_credential_id}
          </AppLink>
          . Its source is maintained by the integration; changing it will break token delivery.
        </div>
      )}

      <RoleGrantsSection kind={secret.kind} id={secret.id} />
      <SourcesSection segments={secret.source_segments} />
      <ConfigurationSection rows={secret.configuration} />
      <SignedHeadersSection headers={secret.signed_headers} />
      <RequestRulesSection rules={secret.rules} />
    </>
  );
}

function EditActions({ kind, secret }: { kind: FormKind; secret: Detail }) {
  const destroy = useDeleteSecret(kind, secret.id);
  const navigate = useNavigate();
  const onDelete = async () => {
    try {
      await destroy.mutateAsync();
      toast.success("Secret deleted.");
      navigate("/console/secrets");
    } catch (error) {
      toast.error(errorMessage(error) || "Could not delete secret.");
    }
  };
  return (
    <div className="ml-auto flex items-center gap-2">
      <Button asChild variant="secondary" size="sm">
        <Link to={`/console/secrets/${kind}/${secret.id}/edit`}>Edit</Link>
      </Button>
      <ConfirmDialog
        title="Delete this secret?"
        description="Any grants of it are removed too. This cannot be undone."
        confirmLabel="Delete"
        onConfirm={onDelete}
        trigger={
          <Button variant="destructive" size="sm">
            Delete
          </Button>
        }
      />
    </div>
  );
}
