import type { ReactNode } from "react";
import { Link } from "react-router";
import { toast } from "sonner";

import { ErrorAlert } from "@/components/alerts";
import { AppLink } from "@/components/app-link";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { errorMessage, navigation } from "@/lib/api";
import { cn } from "@/lib/utils";

import { useIntegrations, useSetupPreset, type IntegrationApp, type IntegrationPreset, type IntegrationUsage } from "../api";
import { ProviderTile } from "../provider-logo";

const USAGE_TEXT: Record<IntegrationUsage, string> = {
  shared: "Shared with your agents.",
  always_available: "Used automatically whenever you ask the bot.",
  direct_messages: "Used only in your DMs with the bot.",
};

export default function IntegrationsPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.integrations}>
      <Integrations />
    </RequireAccess>
  );
}

function Integrations() {
  const query = useIntegrations();

  return (
    <>
      <PageHeader title="Integrations" subtitle="Connect your accounts so Centaur can act on your behalf." />
      {query.error && <ErrorAlert>{errorMessage(query.error)}</ErrorAlert>}
      {query.isLoading && <p className="text-sm text-subtle-foreground">Loading…</p>}
      {query.data && (
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
          <SshKeyCard configured={query.data.ssh_key.configured} />
          {query.data.apps.map((app) => (
            <AppCard key={app.id} app={app} />
          ))}
          {query.data.presets.map((preset) => (
            <PresetCard key={preset.provider} preset={preset} />
          ))}
        </div>
      )}
      <p className="mt-6 text-xs text-subtle-foreground">
        Connecting opens the provider's consent screen. Once you approve, Centaur stores and refreshes the credential
        automatically.
      </p>
    </>
  );
}

function Card({ label, children }: { label: string; children: ReactNode }) {
  return (
    <section aria-label={label} className="flex flex-col gap-3 rounded-sm border border-border bg-card p-5">
      {children}
    </section>
  );
}

function SshKeyCard({ configured }: { configured: boolean }) {
  return (
    <Card label="SSH key">
      <div className="flex items-center gap-3">
        <span
          className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-muted font-mono text-sm text-heading"
          aria-hidden="true"
        >
          &gt;_
        </span>
        <div>
          <h2 className="font-mono text-sm font-semibold text-heading">SSH key</h2>
          <div className="text-[11px] tracking-wide text-muted-foreground uppercase">Remote development</div>
        </div>
      </div>
      <p className="text-xs text-muted-foreground">
        Save one personal key for the SSH destinations your administrator allows. Used in Console and your DMs with the
        bot; channels need an admin grant.
      </p>
      <div className="mt-auto flex items-center justify-between gap-3 pt-1">
        <Button asChild variant={configured ? "secondary" : "default"}>
          <AppLink to={configured ? "/console/ssh_key" : "/console/ssh_key/edit"}>{configured ? "Manage" : "Connect"}</AppLink>
        </Button>
        {configured && <span className="text-xs text-emerald-300 light:text-emerald-700">Configured</span>}
      </div>
    </Card>
  );
}

function AppCard({ app }: { app: IntegrationApp }) {
  const needsReconnect = app.connection?.status === "needs_reconnect";
  return (
    <Card label={app.slug}>
      <div className="flex items-center gap-3">
        <ProviderTile provider={app.provider} />
        <div className="min-w-0">
          <div className="truncate font-mono text-sm font-semibold text-heading" title={app.slug}>
            {app.slug}
          </div>
          <div className="text-[11px] tracking-wide text-muted-foreground uppercase">{app.provider}</div>
        </div>
      </div>
      {app.description && !app.shared && (
        <p className="line-clamp-2 text-xs text-muted-foreground" title={app.description}>
          {app.description}
        </p>
      )}
      <p className="text-xs text-muted-foreground">{USAGE_TEXT[app.usage]}</p>
      <div className="mt-auto flex items-center justify-between gap-3 pt-1">
        {app.connection ? (
          <>
            <div className="flex flex-wrap gap-2">
              {app.shared && (
                <Button asChild variant="secondary">
                  <Link to={`/console/oauth_apps/${app.id}`}>Manage</Link>
                </Button>
              )}
              <Button asChild variant="secondary">
                <a href={app.start_url}>Reconnect</a>
              </Button>
            </div>
            <span
              className={cn(
                "flex items-center gap-1.5 text-xs",
                needsReconnect ? "text-amber-300 light:text-amber-700" : "text-emerald-300 light:text-emerald-700",
              )}
            >
              <span className={cn("size-1.5 rounded-full", needsReconnect ? "bg-amber-400" : "bg-emerald-400")} />
              {needsReconnect ? "Needs reconnecting" : "Connected"}
            </span>
          </>
        ) : (
          <Button asChild>
            <a href={app.start_url}>Connect</a>
          </Button>
        )}
      </div>
    </Card>
  );
}

function PresetCard({ preset }: { preset: IntegrationPreset }) {
  const setup = useSetupPreset();
  const connect = async () => {
    try {
      const body = await setup.mutateAsync(preset.provider);
      navigation.assign(body.data.start_url);
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };
  return (
    <Card label={preset.display_name}>
      <div className="flex items-center gap-3">
        <ProviderTile provider={preset.provider} fallback={false} />
        <h2 className="font-mono text-sm font-semibold text-heading">{preset.display_name}</h2>
      </div>
      {preset.description && <p className="text-xs text-muted-foreground">{preset.description}</p>}
      <div className="mt-auto pt-1">
        <Button type="button" onClick={connect} disabled={setup.isPending}>
          Connect
        </Button>
      </div>
    </Card>
  );
}
