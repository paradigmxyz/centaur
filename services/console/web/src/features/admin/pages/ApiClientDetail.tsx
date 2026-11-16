import { useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { toast } from "sonner";

import { AppLink } from "@/components/app-link";
import { ResourceBadge } from "@/components/badges";
import { BackLink } from "@/components/back-link";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { CopyButton } from "@/components/copy-button";
import { DataTable, type Column } from "@/components/data-table";
import { SectionHeading, useDocumentTitle } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { errorMessage } from "@/lib/api";
import { truncateMiddle } from "@/lib/format";
import { useSubmit } from "@/lib/use-submit";

import {
  attachApiClientPrincipal,
  deleteApiClient,
  detachApiClientPrincipal,
  makeApiClientPrincipalDefault,
  rotateApiClientSecret,
  setApiClientEnabled,
  useApiClient,
  type ApiClientPrincipalLink,
} from "../api";
import { useIssuedSecret } from "../secret-handoff";
import { Chip, ScopeChips } from "../sections/ScopeChips";

const rowButton = "hover:border-primary/40 hover:text-primary";
const dangerButton = "hover:border-destructive/40 hover:text-destructive";

function principalLabel(principal: ApiClientPrincipalLink["principal"]) {
  return principal.name || principal.foreign_id || principal.id;
}

export default function ApiClientDetailPage() {
  const { id = "" } = useParams();
  return (
    <RequireAccess allowed={(me) => me.nav.api_clients}>
      {/* Keyed so an issued secret never carries over to another client. */}
      <ApiClientDetail key={id} id={id} />
    </RequireAccess>
  );
}

function ApiClientDetail({ id }: { id: string }) {
  // Before the suspending query, so the handoff is consumed even if it fails.
  const [secret, setSecret] = useIssuedSecret(id);
  return <ApiClientDetailBody id={id} secret={secret} setSecret={setSecret} />;
}

function ApiClientDetailBody({
  id,
  secret,
  setSecret,
}: {
  id: string;
  secret: string | null;
  setSecret: (secret: string | null) => void;
}) {
  const { data: client } = useApiClient(id);
  const queryClient = useQueryClient();
  const makeDefault = useSubmit((linkId: string) => makeApiClientPrincipalDefault(queryClient, id, linkId));
  const enable = useSubmit(() => setApiClientEnabled(queryClient, id, true));
  const attachPrincipal = useSubmit((ref: string) => attachApiClientPrincipal(queryClient, id, ref));
  const navigate = useNavigate();
  const [principalRef, setPrincipalRef] = useState("");
  useDocumentTitle(client.name);

  const run = async (action: () => Promise<unknown>, success: string) => {
    try {
      await action();
      toast.success(success);
      return true;
    } catch (error) {
      toast.error(errorMessage(error));
      return false;
    }
  };

  const rotate = async () => {
    try {
      const body = await rotateApiClientSecret(queryClient, id);
      // The plaintext lives only in the parent's state.
      setSecret(body.data.secret);
      toast.success("Secret rotated. The previous secret no longer works.");
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };

  const destroy = async () => {
    if (await run(() => deleteApiClient(queryClient, id), `Deleted API client ${client.name}.`)) {
      navigate("/console/api_clients");
    }
  };

  const attach = async (event: FormEvent) => {
    event.preventDefault();
    const ref = principalRef.trim();
    if (!ref) return;
    try {
      const body = await attachPrincipal.run(ref);
      setPrincipalRef("");
      toast.success(`Attached ${principalLabel(body.data.principal)}.`);
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };

  const columns: Column<ApiClientPrincipalLink>[] = [
    {
      id: "id",
      header: "ID",
      className: "font-mono text-xs whitespace-nowrap",
      cell: ({ principal }) => (
        <>
          <AppLink
            to={`/console/principals/${principal.id}`}
            className="text-primary hover:underline"
          >
            {principal.id}
          </AppLink>
          {principal.foreign_id && (
            <div className="text-xs text-muted-foreground" title={principal.foreign_id}>
              {truncateMiddle(principal.foreign_id)}
            </div>
          )}
        </>
      ),
    },
    { id: "name", header: "Name", className: "text-foreground", cell: ({ principal }) => principal.name || "unnamed" },
    {
      id: "kind",
      header: "Kind",
      className: "whitespace-nowrap",
      cell: ({ principal }) => <ResourceBadge>{principal.kind}</ResourceBadge>,
    },
    {
      id: "default",
      header: <span className="sr-only">Default</span>,
      className: "whitespace-nowrap",
      cell: (link) => link.default && <Chip>default</Chip>,
    },
    {
      id: "actions",
      header: <span className="sr-only">Actions</span>,
      className: "whitespace-nowrap",
      cell: (link) => (
        <div className="flex items-center justify-end gap-2">
          {!link.default && (
            <Button
              variant="secondary"
              size="sm"
              className={rowButton}
              disabled={makeDefault.pending}
              onClick={() =>
                void run(
                  () => makeDefault.run(link.id),
                  `${principalLabel(link.principal)} is now the default principal.`,
                )
              }
            >
              Make default
            </Button>
          )}
          <ConfirmDialog
            title="Detach this principal?"
            description="Sessions already bound to it stop accepting turns."
            confirmLabel="Detach"
            onConfirm={() =>
              run(() => detachApiClientPrincipal(queryClient, id, link.id), `Detached ${principalLabel(link.principal)}.`)
            }
            trigger={
              <Button variant="secondary" size="sm" className={dangerButton}>
                Detach
              </Button>
            }
          />
        </div>
      ),
    },
  ];

  return (
    <>
      <div className="mb-8">
        <BackLink to="/console/api_clients">back to API clients</BackLink>
        <div className="mt-2 flex flex-wrap items-center gap-3">
          <h1 className="text-2xl font-semibold tracking-tight">{client.name}</h1>
          <div className="ml-auto flex flex-wrap items-center gap-2">
            <Button asChild variant="secondary" size="sm">
              <Link to={`/console/api_clients/${client.id}/edit`}>Edit</Link>
            </Button>
            <ConfirmDialog
              title="Rotate the secret?"
              description="The current secret and its access tokens stop working immediately."
              confirmLabel="Rotate secret"
              onConfirm={rotate}
              trigger={
                <Button variant="secondary" size="sm">
                  Rotate secret
                </Button>
              }
            />
            {client.disabled ? (
              <Button
                variant="secondary"
                size="sm"
                disabled={enable.pending}
                onClick={() => void run(() => enable.run(), "API client enabled.")}
              >
                Enable
              </Button>
            ) : (
              <ConfirmDialog
                title="Disable this client?"
                description="Its access tokens stop working immediately."
                confirmLabel="Disable"
                onConfirm={() => run(() => setApiClientEnabled(queryClient, id, false), "API client disabled.")}
                trigger={
                  <Button variant="secondary" size="sm">
                    Disable
                  </Button>
                }
              />
            )}
            <ConfirmDialog
              title={`Delete ${client.name}?`}
              description="This cannot be undone."
              confirmLabel="Delete"
              onConfirm={destroy}
              trigger={
                <Button variant="secondary" size="sm" className={dangerButton}>
                  Delete
                </Button>
              }
            />
          </div>
        </div>
        <div className="mt-2 flex flex-wrap items-center gap-2 text-sm">
          <span className="text-muted-foreground">client_id:</span>
          <span className="rounded-md border bg-muted/50 px-1.5 py-0.5 font-mono text-xs text-foreground">{client.id}</span>
          <span className="text-muted-foreground">·</span>
          <span className="text-muted-foreground">{client.disabled ? "disabled" : "active"}</span>
        </div>
        <div className="mt-3">
          <ScopeChips scopes={client.scopes} />
        </div>
      </div>

      {secret && (
        <section
          className="mb-8 rounded-xl border border-warning/30 bg-warning/10 p-5"
          aria-labelledby="api-client-secret-heading"
        >
          <h2 id="api-client-secret-heading" className="mb-2 text-sm font-semibold text-warning">
            Client secret
          </h2>
          <p className="mb-3 text-sm text-muted-foreground">
            Copy it now. It is shown only once; rotate the secret if it is lost.
          </p>
          <div className="flex items-center gap-2">
            <label className="sr-only" htmlFor="api_client_secret">
              Client secret
            </label>
            <Input
              id="api_client_secret"
              readOnly
              value={secret}
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              onFocus={(event) => event.currentTarget.select()}
            />
            <CopyButton value={secret} label="Copy secret" />
          </div>
        </section>
      )}

      <section aria-labelledby="api-client-principals-heading">
        <div className="mb-4 flex flex-wrap items-end justify-between gap-3">
          <div>
            <SectionHeading>
              <span id="api-client-principals-heading">Principals</span>
            </SectionHeading>
            <p className="text-sm text-muted-foreground">
              A session runs as the principal it names, or the default. Its sandbox gets that principal&apos;s grants.
            </p>
          </div>
          <form onSubmit={attach} className="flex flex-wrap items-center gap-3">
            <label className="sr-only" htmlFor="attach_principal">
              Principal OID or foreign ID
            </label>
            <Input
              id="attach_principal"
              className="w-80 max-w-full"
              placeholder="prn_… or foreign ID"
              required
              value={principalRef}
              onChange={(event) => setPrincipalRef(event.target.value)}
            />
            <Button type="submit" disabled={attachPrincipal.pending}>
              Attach principal
            </Button>
          </form>
        </div>

        <DataTable
          caption="Attached principals"
          columns={columns}
          rows={client.principals}
          rowKey={(link) => link.id}
          empty="No principals attached. The client cannot create sessions until one is attached."
        />
      </section>
    </>
  );
}
