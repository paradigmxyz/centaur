import { Link, useNavigate, useParams } from "react-router";
import { toast } from "sonner";

import { AppLink } from "@/components/app-link";
import { BackLink } from "@/components/back-link";
import { LabelChips, StatusBadge, credentialStatusTone } from "@/components/badges";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { LocalTime } from "@/components/local-time";
import { useDocumentTitle } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { ApiError, errorMessage } from "@/lib/api";
import { displayLabel } from "@/lib/format";

import { credentialTitle, useCredential, useDeleteCredential } from "../api";
import { DefinitionSection } from "../sections/DefinitionSection";

const linkClass = "text-centaur-400 hover:text-centaur-300 hover:underline light:text-primary";
const REDACTED = "[redacted]";

export default function CredentialDetailPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.credentials}>
      <CredentialDetail />
    </RequireAccess>
  );
}

function CredentialDetail() {
  const { id = "" } = useParams();
  const { data: credential } = useCredential(id);
  const destroy = useDeleteCredential(id);
  const navigate = useNavigate();
  const title = credentialTitle(credential);
  useDocumentTitle(title);

  const onDelete = async () => {
    try {
      await destroy.mutateAsync();
      toast.success("Credential deleted.");
      navigate("/console/credentials");
    } catch (error) {
      // 409: a token_broker source still references the credential.
      const message = error instanceof ApiError && error.status === 409 ? error.message : errorMessage(error);
      toast.error(message || "Could not delete credential.");
    }
  };

  const { grant } = credential;
  const headerNames = credential.token_endpoint_header_names;

  return (
    <>
      <div className="mb-8">
        <BackLink to="/console/credentials">back to credentials</BackLink>
        <div className="mt-2 flex flex-wrap items-center gap-3">
          <h1 className="text-2xl font-bold text-heading">{title}</h1>
          <StatusBadge tone={credentialStatusTone(credential.status)}>{credential.status}</StatusBadge>
          <div className="ml-auto flex items-center gap-2">
            <Button asChild variant="secondary" size="sm">
              <Link to={`/console/credentials/${credential.id}/edit`}>Edit</Link>
            </Button>
            <ConfirmDialog
              title="Delete this credential?"
              description="This cannot be undone."
              confirmLabel="Delete"
              onConfirm={onDelete}
              trigger={
                <Button variant="destructive" size="sm">
                  Delete
                </Button>
              }
            />
          </div>
        </div>
        <div className="mt-2 flex flex-wrap items-center gap-2 text-sm">
          <span className="glow text-centaur-400 light:text-primary">{credential.id}</span>
          {credential.foreign_id && (
            <>
              <span className="text-subtle-foreground">·</span>
              <span className="text-zinc-400">
                foreign_id: <span className="break-all text-heading">{credential.foreign_id}</span>
              </span>
            </>
          )}
        </div>
        {credential.description && (
          <p className="mt-3 max-w-2xl text-sm text-muted-foreground">{credential.description}</p>
        )}
        <div className="mt-3">
          <LabelChips labels={credential.labels} />
        </div>
      </div>

      <DefinitionSection
        title="Refresh"
        rows={[
          ["Last refresh", <LocalTime value={credential.last_refresh} format="relative" />],
          ["Expires", credential.expires_at && <LocalTime value={credential.expires_at} />],
          ["Next attempt", credential.next_attempt_at && <LocalTime value={credential.next_attempt_at} />],
          ["Failure count", String(credential.failure_count)],
          credential.dead && ["Dead reason", credential.dead_reason],
        ]}
      />

      <DefinitionSection
        title="Token Client"
        rows={[
          ["Grant", grant],
          Boolean(credential.effective_client_id) && ["Client ID", credential.effective_client_id],
          ["Token endpoint", credential.token_endpoint],
          ["Scopes", credential.scopes.join("\n")],
          grant === "password" && credential.has_username && ["Username", REDACTED],
          grant === "password" && credential.has_password && ["Password", REDACTED],
          grant === "preqin" && credential.has_username && ["Username", REDACTED],
          grant === "preqin" && credential.has_api_key && ["API key", REDACTED],
          headerNames.length > 0 && [
            "Token endpoint headers",
            headerNames.map((name) => `${name}: ${REDACTED}`).join("\n"),
          ],
        ]}
      />

      <DefinitionSection
        title="Refresh Policy"
        rows={[
          ["Early refresh fraction", String(credential.early_refresh_fraction)],
          ["Early refresh slack", `${credential.early_refresh_slack_seconds}s`],
          ["Max refresh interval", `${credential.max_refresh_interval_seconds}s`],
          ["Refresh timeout", `${credential.refresh_timeout_seconds}s`],
        ]}
      />

      {credential.oauth_app && (
        <DefinitionSection
          title="Provider Identity"
          rows={[
            [
              "OAuth app",
              <AppLink to={`/console/oauth_apps/${credential.oauth_app.id}`} className={linkClass}>
                {credential.oauth_app.slug}
              </AppLink>,
            ],
            ["Provider", credential.oauth_app.provider],
            ["Provider email", credential.provider_email],
            ["Provider subject", credential.provider_subject],
            ["End-user key", credential.external_user_key],
          ]}
          note="Client id and secret are inherited from the OAuth app; rotating the app's secret applies here."
        />
      )}

      {credential.static_secret && (
        <DefinitionSection
          title="Grantable As"
          rows={[
            [
              "Static secret",
              <AppLink to={`/console/secrets/static/${credential.static_secret.id}`} className={linkClass}>
                {displayLabel(credential.static_secret)}
              </AppLink>,
            ],
          ]}
          note="Grant this credential to a principal by granting the static secret above; it injects the live token into matching requests."
        />
      )}

      <DefinitionSection
        title="Metadata"
        rows={[
          ["Created", <LocalTime value={credential.created_at} />],
          ["Updated", <LocalTime value={credential.updated_at} />],
        ]}
      />

      <p className="text-xs text-subtle-foreground">
        Token material (access token, refresh token, client secret) is never shown.
      </p>
    </>
  );
}
