import { useNavigate, useParams } from "react-router";
import { toast } from "sonner";

import { ErrorAlert } from "@/components/alerts";
import { ResourceBadge } from "@/components/badges";
import { BackLink } from "@/components/back-link";
import { useDocumentTitle } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { errorMessage } from "@/lib/api";

import { useOauthApp, useOauthProviders, useUpdateOauthApp } from "../api";
import { OauthAppErrors } from "../sections/OauthAppErrors";
import { OauthAppForm } from "../sections/OauthAppForm";

export default function OauthAppEditPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.oauth_apps}>
      <OauthAppEdit />
    </RequireAccess>
  );
}

function OauthAppEdit() {
  const { id = "" } = useParams();
  const { data: app } = useOauthApp(id);
  useDocumentTitle(`Edit ${app.slug}`);
  const navigate = useNavigate();
  const providers = useOauthProviders();
  const update = useUpdateOauthApp(id);

  return (
    <>
      <div className="mb-8">
        <BackLink to={`/console/oauth_apps/${app.id}`}>back to App</BackLink>
        <div className="mt-2 flex flex-wrap items-center gap-3">
          <h1 className="text-2xl font-bold text-heading">Edit {app.slug}</h1>
          <ResourceBadge>OAuth</ResourceBadge>
        </div>
      </div>
      <OauthAppErrors error={update.error} />
      {providers.error ? (
        <ErrorAlert>{errorMessage(providers.error)}</ErrorAlert>
      ) : !providers.data ? (
        <p className="text-sm text-subtle-foreground">Loading…</p>
      ) : (
        <OauthAppForm
          app={app}
          providers={providers.data}
          submitLabel="Save changes"
          cancelTo="/console/oauth_apps"
          onSubmit={async (payload) => {
            await update.mutateAsync(payload);
            toast.success("OAuth app updated.");
            navigate(`/console/oauth_apps/${app.id}`);
          }}
        />
      )}
    </>
  );
}
