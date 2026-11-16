import { Link, useParams } from "react-router";

import { LabelChips } from "@/components/badges";
import { BackLink } from "@/components/back-link";
import { CopyButton } from "@/components/copy-button";
import { useDocumentTitle } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { humanize } from "@/lib/api";

import { useOauthApp } from "../api";
import { EnabledBadge, ProviderPill } from "../sections/AppBadges";
import { DefinitionSection } from "../sections/DefinitionSection";
import { MintedCredentialsSection } from "../sections/MintedCredentialsSection";
import { ProviderPanel } from "../sections/ProviderPanel";

export default function OauthAppDetailPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.oauth_apps}>
      <OauthAppDetail />
    </RequireAccess>
  );
}

function Copyable({ value, label }: { value: string; label: string }) {
  return (
    <span className="flex flex-wrap items-center gap-2">
      <span>{value}</span>
      <CopyButton value={value} label={label} />
    </span>
  );
}

function OauthAppDetail() {
  const { id = "" } = useParams();
  const { data: app } = useOauthApp(id);
  useDocumentTitle(app.slug);

  return (
    <>
      <div className="mb-8">
        <BackLink to="/console/oauth_apps">back to Apps</BackLink>
        <div className="mt-2 flex flex-wrap items-center gap-3">
          <h1 className="font-mono text-2xl font-semibold tracking-tight">{app.slug}</h1>
          <EnabledBadge enabled={app.enabled} />
          <Button asChild variant="secondary" className="ml-auto">
            <Link to={`/console/oauth_apps/${app.id}/edit`}>Edit</Link>
          </Button>
        </div>
        <div className="mt-2 flex flex-wrap items-center gap-2 text-sm">
          <span className="rounded-md border bg-muted/50 px-1.5 py-0.5 font-mono text-xs text-foreground">{app.id}</span>
          <span className="text-muted-foreground">·</span>
          <ProviderPill provider={app.provider} />
        </div>
        {app.description && <p className="mt-3 max-w-2xl text-sm text-muted-foreground">{app.description}</p>}
        <div className="mt-3">
          <LabelChips labels={app.labels} />
        </div>
      </div>

      <ProviderPanel app={app} />

      <DefinitionSection
        title="OAuth Client"
        rows={[
          ["Provider", app.provider],
          ["Client ID", app.client_id],
          ["Client secret", app.client_secret_set ? "set" : "not set"],
          ...Object.entries(app.provider_config).map(([key, value]): [string, string] => [humanize(key), value]),
        ]}
      />
      <DefinitionSection title="Flow Config" rows={[["Allowed scopes", app.allowed_scopes.join("\n")]]} />
      <DefinitionSection
        title="Consent Flow"
        rows={[
          ["Redirect URI", <Copyable key="redirect" value={app.redirect_uri} label="Copy redirect URI" />],
          ["Start URL", <Copyable key="start" value={app.start_url} label="Copy start URL" />],
        ]}
        footer={`Register the redirect URI with the provider. ${
          app.shared
            ? "An admin must connect this shared account."
            : "Share the start URL with a team member to connect the integration."
        }`}
      />

      <MintedCredentialsSection appId={app.id} />

      <p className="text-xs text-muted-foreground">
        The client secret is never shown. Credentials minted by this app inherit its client id and secret.
      </p>
    </>
  );
}
