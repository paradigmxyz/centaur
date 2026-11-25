// The signed-in admin's own bearer API keys (for centaur-perms and other
// operator tooling). A new key's token is held only in this page's state and
// shown once.
import { useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";

import { ErrorAlert } from "@/components/alerts";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { ControlTabs } from "@/components/control-tabs";
import { CopyButton } from "@/components/copy-button";
import { DataTable, type Column } from "@/components/data-table";
import { LocalTime } from "@/components/local-time";
import { PageHeader } from "@/components/page-header";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { errorMessage } from "@/lib/api";
import { useSubmit } from "@/lib/use-submit";

import { createApiKey, revokeApiKey, useApiKeys, type ApiKey, type IssuedApiKey } from "../api";

export default function ApiKeysPageRoute() {
  return (
    <RequireAccess allowed={(me) => me.nav.api_keys}>
      <ApiKeysPage />
    </RequireAccess>
  );
}

function ApiKeysPage() {
  const keys = useApiKeys();
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [issued, setIssued] = useState<IssuedApiKey | null>(null);
  const create = useSubmit((keyName: string) => createApiKey(queryClient, keyName));
  const revoke = useSubmit((id: string) => revokeApiKey(queryClient, id));

  const onCreate = async (event: FormEvent) => {
    event.preventDefault();
    try {
      setIssued(await create.run(name.trim()));
      setName("");
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };

  const onRevoke = async (key: ApiKey) => {
    try {
      await revoke.run(key.id);
      if (issued?.id === key.id) setIssued(null);
      toast.success(`Revoked ${key.name}.`);
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };

  const columns: Column<ApiKey>[] = [
    { id: "name", header: "Name", cell: (key) => <span className="font-medium">{key.name}</span> },
    {
      id: "created",
      header: "Created",
      className: "whitespace-nowrap text-muted-foreground",
      cell: (key) => <LocalTime value={key.created_at} format="relative" />,
    },
    {
      id: "actions",
      header: <span className="sr-only">Actions</span>,
      className: "whitespace-nowrap",
      cell: (key) => (
        <div className="flex justify-end">
          <ConfirmDialog
            title={`Revoke ${key.name}?`}
            description="Anything using this key stops working immediately."
            confirmLabel="Revoke"
            onConfirm={() => onRevoke(key)}
            trigger={
              <Button variant="secondary" size="sm" className="hover:border-destructive/40 hover:text-destructive">
                Revoke
              </Button>
            }
          />
        </div>
      ),
    },
  ];

  return (
    <>
      <ControlTabs />
      <PageHeader
        title="API Keys"
        subtitle="Your bearer keys for the Console API, used by centaur-perms and other operator tooling."
      />

      <form onSubmit={(event) => void onCreate(event)} className="mb-6 flex max-w-xl items-end gap-2">
        <div className="flex-1">
          <label htmlFor="api_key_name" className="mb-1 block text-sm font-medium">
            New key name
          </label>
          <Input
            id="api_key_name"
            value={name}
            placeholder="e.g. laptop centaur-perms"
            onChange={(event) => setName(event.target.value)}
          />
        </div>
        <Button type="submit" disabled={create.pending || name.trim() === ""}>
          Create key
        </Button>
      </form>

      {issued && (
        <section
          className="mb-8 rounded-xl border border-warning/30 bg-warning/10 p-5"
          aria-labelledby="api-key-token-heading"
        >
          <h2 id="api-key-token-heading" className="mb-2 text-sm font-semibold text-warning">
            {issued.name}
          </h2>
          <p className="mb-3 text-sm text-muted-foreground">
            Copy it now. It is shown only once; create a new key if it is lost.
          </p>
          <div className="flex items-center gap-2">
            <label className="sr-only" htmlFor="api_key_token">
              API key
            </label>
            <Input
              id="api_key_token"
              readOnly
              value={issued.token}
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              onFocus={(event) => event.currentTarget.select()}
            />
            <CopyButton value={issued.token} label="Copy key" />
          </div>
        </section>
      )}

      {keys.isError && <ErrorAlert>{errorMessage(keys.error)}</ErrorAlert>}
      {!keys.isError && (
        <DataTable
          caption="API keys"
          columns={columns}
          rows={keys.data}
          loading={keys.isLoading}
          rowKey={(key) => key.id}
          empty="No API keys yet."
        />
      )}
    </>
  );
}
