import { useNavigate } from "react-router";
import { toast } from "sonner";

import { ErrorAlert } from "@/components/alerts";
import { ResourceBadge } from "@/components/badges";
import { BackLink } from "@/components/back-link";
import { useDocumentTitle } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { errorMessage } from "@/lib/api";

import { useCreateOauthApp, useOauthProviders } from "../api";
import { OauthAppErrors } from "../sections/OauthAppErrors";
import { OauthAppForm } from "../sections/OauthAppForm";

export default function OauthAppNewPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.oauth_apps}>
      <OauthAppNew />
    </RequireAccess>
  );
}

function OauthAppNew() {
  useDocumentTitle("New App");
  const navigate = useNavigate();
  const providers = useOauthProviders();
  const create = useCreateOauthApp();

  return (
    <>
      <div className="mb-8">
        <BackLink to="/console/oauth_apps">back to Apps</BackLink>
        <div className="mt-2 flex flex-wrap items-center gap-3">
          <h1 className="text-2xl font-bold text-heading">New App</h1>
          <ResourceBadge>OAuth</ResourceBadge>
        </div>
      </div>
      <OauthAppErrors error={create.error} />
      {providers.error ? (
        <ErrorAlert>{errorMessage(providers.error)}</ErrorAlert>
      ) : !providers.data ? (
        <p className="text-sm text-subtle-foreground">Loading…</p>
      ) : (
        <OauthAppForm
          providers={providers.data}
          submitLabel="Create app"
          cancelTo="/console/oauth_apps"
          onSubmit={async (payload) => {
            const body = await create.mutateAsync(payload);
            toast.success("OAuth app created.");
            navigate(`/console/oauth_apps/${body.data.id}`);
          }}
        />
      )}
    </>
  );
}
