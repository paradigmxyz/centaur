import { useQueryClient } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";
import { Link } from "react-router";
import { toast } from "sonner";

import { ErrorAlert } from "@/components/alerts";
import { AppLink } from "@/components/app-link";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { errorMessage, navigation } from "@/lib/api";
import { cn } from "@/lib/utils";

import {
  disconnectIntegration,
  setupPreset,
  useIntegrations,
  type DisconnectOutcome,
  type IntegrationApp,
  type IntegrationPreset,
  type IntegrationUsage,
} from "../api";
import { ProviderTile } from "../provider-logo";

const USAGE_TEXT: Record<IntegrationUsage, string> = {
  shared: "Shared with your agents.",
  always_available: "Used automatically whenever you ask the bot.",
  direct_messages: "Used only in your DMs with the bot.",
};

const OWNER_ONLY_TEXT = "Your own connection: it is never shared with anyone else.";

const DISCONNECT_TEXT: Record<DisconnectOutcome, (slug: string) => string> = {
  revoked: (slug) => `${slug} disconnected.`,
  local_only: (slug) =>
    `${slug} disconnected from Centaur. The provider offers no revocation endpoint; revoke access there if needed.`,
  failed: (slug) => `${slug} disconnected from Centaur. Remote revocation failed; revoke the connection with the provider too.`,
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
      {query.isError && <ErrorAlert>{errorMessage(query.error)}</ErrorAlert>}
      {query.isLoading && <p className="text-sm text-muted-foreground">Loading…</p>}
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
      <p className="mt-6 text-xs text-muted-foreground">
        Connecting opens the provider's consent screen. Once you approve, Centaur stores and refreshes the credential
        automatically.
      </p>
    </>
  );
}

function Card({ label, children }: { label: string; children: ReactNode }) {
  return (
    <section aria-label={label} className="flex flex-col gap-3 rounded-xl border bg-card p-5 shadow-xs transition-colors hover:border-foreground/15">
      {children}
    </section>
  );
}

function SshKeyCard({ configured }: { configured: boolean }) {
  return (
    <Card label="SSH key">
      <div className="flex items-center gap-3">
        <span
          className="flex size-10 shrink-0 items-center justify-center rounded-lg border bg-muted/50 font-mono text-sm"
          aria-hidden="true"
        >
          &gt;_
        </span>
        <div>
          <h2 className="text-sm font-semibold">SSH key</h2>
          <div className="text-xs text-muted-foreground">Remote development</div>
        </div>
      </div>
      <p className="text-xs text-muted-foreground">
        Save one personal key for the SSH destinations your administrator allows. Used in Console and your DMs with the
        bot; channels need an admin grant.
      </p>
      <div className="mt-auto flex items-center justify-between gap-3 pt-1">
        <Button asChild variant={configured ? "secondary" : "outline"}>
          <AppLink to={configured ? "/console/ssh_key" : "/console/ssh_key/edit"}>{configured ? "Manage" : "Connect"}</AppLink>
        </Button>
        {configured && <span className="text-xs text-success">Configured</span>}
      </div>
    </Card>
  );
}

function AppCard({ app }: { app: IntegrationApp }) {
  const queryClient = useQueryClient();
  const needsReconnect = app.connection?.status === "needs_reconnect";
  const connectionId = app.connection?.id;
  const disconnect = async () => {
    if (!connectionId) return;
    try {
      const outcome = await disconnectIntegration(queryClient, connectionId);
      const message = DISCONNECT_TEXT[outcome](app.slug);
      if (outcome === "failed") toast.warning(message);
      else toast.success(message);
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };
  return (
    <Card label={app.slug}>
      <div className="flex items-center gap-3">
        <ProviderTile provider={app.provider} />
        <div className="min-w-0">
          <div className="truncate text-sm font-semibold" title={app.slug}>
            {app.slug}
          </div>
          <div className="font-mono text-xs text-muted-foreground">{app.provider}</div>
        </div>
      </div>
      {app.description && !app.shared && (
        <p className="line-clamp-2 text-xs text-muted-foreground" title={app.description}>
          {app.description}
        </p>
      )}
      <p className="text-xs text-muted-foreground">
        {USAGE_TEXT[app.usage]}
        {app.owner_only && ` ${OWNER_ONLY_TEXT}`}
      </p>
      <div className="mt-auto flex flex-wrap items-center justify-between gap-3 pt-1">
        {app.connection ? (
          <>
            <div className="flex gap-2">
              {app.shared && (
                <Button asChild variant="secondary">
                  <Link to={`/console/oauth_apps/${app.id}`}>Manage</Link>
                </Button>
              )}
              <Button asChild variant="secondary">
                <a href={app.start_url}>Reconnect</a>
              </Button>
              {app.connection.can_disconnect && (
                <ConfirmDialog
                  title={`Disconnect ${app.slug}?`}
                  description="Centaur stops using this connection right away and asks the provider to revoke its tokens. Connect again at any time."
                  confirmLabel="Disconnect"
                  onConfirm={disconnect}
                  trigger={<Button variant="outline">Disconnect</Button>}
                />
              )}
            </div>
            <span
              className={cn(
                "ml-auto flex items-center gap-1.5 text-xs",
                needsReconnect ? "text-warning" : "text-success",
              )}
            >
              <span className={cn("size-1.5 rounded-full", needsReconnect ? "bg-warning" : "bg-success")} />
              {needsReconnect ? "Needs reconnecting" : "Connected"}
            </span>
          </>
        ) : (
          <Button asChild variant="outline">
            <a href={app.start_url}>Connect</a>
          </Button>
        )}
      </div>
    </Card>
  );
}

function PresetCard({ preset }: { preset: IntegrationPreset }) {
  const queryClient = useQueryClient();
  const [pending, setPending] = useState(false);
  const [input, setInput] = useState<Record<string, string>>({});
  const missingInput = preset.inputs.some((field) => !(input[field.name] ?? "").trim());
  const connect = async () => {
    setPending(true);
    try {
      const body = await setupPreset(queryClient, preset.provider, input);
      navigation.assign(body.data.start_url);
    } catch (error) {
      toast.error(errorMessage(error));
    } finally {
      setPending(false);
    }
  };
  return (
    <Card label={preset.display_name}>
      <div className="flex items-center gap-3">
        <ProviderTile provider={preset.provider} fallback={false} />
        <h2 className="text-sm font-semibold">{preset.display_name}</h2>
      </div>
      {preset.description && <p className="text-xs text-muted-foreground">{preset.description}</p>}
      {preset.inputs.map((field) => {
        const id = `preset-${preset.provider}-${field.name}`;
        return (
          <div key={field.name} className="grid gap-1.5">
            <Label htmlFor={id} className="text-xs">
              {field.label}
            </Label>
            <Input
              id={id}
              type={field.type}
              placeholder={field.placeholder ?? undefined}
              value={input[field.name] ?? ""}
              onChange={(event) => setInput((current) => ({ ...current, [field.name]: event.target.value }))}
            />
          </div>
        );
      })}
      <div className="mt-auto pt-1">
        <Button type="button" variant="outline" onClick={connect} disabled={pending || missingInput}>
          Connect
        </Button>
      </div>
    </Card>
  );
}
