import { ChevronDown, Search } from "lucide-react";
import { useState, type FormEvent } from "react";
import { Link, useSearchParams } from "react-router";

import { ResourceBadge, RolePill } from "@/components/badges";
import { ControlTabs } from "@/components/control-tabs";
import { DataTable, type Column } from "@/components/data-table";
import { LocalTime } from "@/components/local-time";
import { PageHeader } from "@/components/page-header";
import { usePageParam } from "@/components/pagination";
import { RequireAccess } from "@/components/require-access";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { pluralize, truncateMiddle } from "@/lib/format";
import { SECRET_KIND_SLUGS, secretKindLabel } from "@/lib/secret-kinds";
import { cn } from "@/lib/utils";

import { FORM_KINDS, useSecretList, useSecretTotal, type SecretListItem } from "../api";
import { QueryErrorAlert } from "../query-error";

const ALL_TYPES = "all";

const columns: Column<SecretListItem>[] = [
  {
    id: "name",
    header: "Name",
    className: "w-[38%] overflow-hidden text-heading",
    cell: (secret) => {
      const name = secret.name || "unnamed";
      return (
        <div className="flex min-w-0 items-center gap-2">
          <Link
            to={`/console/secrets/${secret.kind}/${secret.id}`}
            title={name}
            className={cn(
              "block min-w-0 truncate text-centaur-400 hover:text-centaur-300 hover:underline light:text-primary",
              !secret.name && "text-subtle-foreground",
            )}
          >
            {truncateMiddle(name, 64)}
          </Link>
          {secret.managed_by && (
            <span
              title={`Managed by the ${secret.managed_by.oauth_app_slug ?? ""} OAuth integration`}
              className="inline-flex shrink-0 items-center rounded-sm bg-primary/10 px-1.5 py-0.5 text-[11px] text-centaur-300 ring-1 ring-primary/30 light:text-primary"
            >
              managed
            </span>
          )}
        </div>
      );
    },
  },
  {
    id: "type",
    header: "Type",
    className: "w-[20%] overflow-hidden whitespace-nowrap",
    cell: (secret) => <ResourceBadge>{secretKindLabel(secret.kind)}</ResourceBadge>,
  },
  {
    id: "source",
    header: "Source",
    className: "w-[27%] overflow-hidden",
    cell: (secret) =>
      secret.source_types.length === 0 ? (
        <span className="text-subtle-foreground">—</span>
      ) : (
        <div className="flex min-w-0 flex-wrap gap-1">
          {secret.source_types.map((type) => (
            <RolePill key={type}>{type}</RolePill>
          ))}
        </div>
      ),
  },
  {
    id: "created",
    header: "Created",
    className: "w-[15%] whitespace-nowrap text-xs text-muted-foreground",
    cell: (secret) => <LocalTime value={secret.created_at} format="compact" />,
  },
];

export default function SecretsListPage() {
  return (
    <RequireAccess allowed={(me) => me.nav.secrets}>
      <SecretsList />
    </RequireAccess>
  );
}

function SecretsList() {
  const [params] = useSearchParams();
  const q = params.get("q") ?? "";
  const rawType = params.get("type") ?? "";
  // Unknown types are ignored, like the legacy page.
  const type = (SECRET_KIND_SLUGS as string[]).includes(rawType) ? rawType : "";
  const [page, setPage] = usePageParam();
  const list = useSecretList({ q, kind: type, page });
  const total = useSecretTotal();
  const filtered = Boolean(q || type);

  return (
    <>
      <ControlTabs />
      <PageHeader
        title="Secrets"
        subtitle={total.data === undefined ? " " : `${pluralize(total.data, "secret")} total.`}
        actions={<AddSecretMenu />}
      />
      <SecretFilters q={q} type={type} />
      {list.isError ? (
        <QueryErrorAlert what="secrets" error={list.error} onRetry={() => void list.refetch()} />
      ) : (
        total.isError && (
          <QueryErrorAlert what="the secret count" error={total.error} onRetry={() => void total.refetch()} />
        )
      )}
      {/* A failed request shows the alert, never the empty state; stale non-empty rows stay after a failed refetch. */}
      {(!list.isError || (list.data?.data.length ?? 0) > 0) && (
        <DataTable
          fixed
          caption="Secrets"
          columns={columns}
          rows={list.data?.data}
          loading={list.isLoading}
          rowKey={(secret) => `${secret.kind}:${secret.id}`}
          empty={filtered ? "No secrets match these filters." : "No secrets yet."}
          pagination={list.data ? { meta: list.data.meta, unit: "secret", onPageChange: setPage } : undefined}
        />
      )}
    </>
  );
}

function AddSecretMenu() {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button>
          + Add Secret
          <ChevronDown aria-hidden="true" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-48">
        {FORM_KINDS.map((kind) => (
          <DropdownMenuItem key={kind} asChild>
            <Link to={`/console/secrets/${kind}/new`}>{secretKindLabel(kind)}</Link>
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** The q + type filter form (GET-style: the URL holds the filters). */
function SecretFilters({ q, type }: { q: string; type: string }) {
  const [, setParams] = useSearchParams();
  const [query, setQuery] = useState(q);
  const [kind, setKind] = useState(type || ALL_TYPES);
  const [synced, setSynced] = useState(`${q}|${type}`);
  if (synced !== `${q}|${type}`) {
    setSynced(`${q}|${type}`);
    setQuery(q);
    setKind(type || ALL_TYPES);
  }

  const apply = (nextQ: string, nextType: string) =>
    setParams(() => {
      const next = new URLSearchParams();
      if (nextQ.trim()) next.set("q", nextQ.trim());
      if (nextType && nextType !== ALL_TYPES) next.set("type", nextType);
      return next;
    });

  const onSubmit = (event: FormEvent) => {
    event.preventDefault();
    apply(query, kind);
  };

  return (
    <form role="search" onSubmit={onSubmit} className="mb-3 flex flex-wrap items-center gap-2">
      <div className="relative w-full max-w-md">
        <span className="pointer-events-none absolute inset-y-0 left-0 flex items-center pl-3 text-muted-foreground">
          <Search className="size-4" aria-hidden="true" />
        </span>
        <Input
          className="pl-9"
          type="search"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search secret names"
          aria-label="Search secrets by name"
        />
      </div>
      <Select value={kind} onValueChange={setKind}>
        <SelectTrigger className="w-auto" aria-label="Filter secrets by type">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={ALL_TYPES}>All types</SelectItem>
          {SECRET_KIND_SLUGS.map((slug) => (
            <SelectItem key={slug} value={slug}>
              {secretKindLabel(slug)}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <Button type="submit" variant="secondary">
        Filter
      </Button>
      {(q || type) && (
        <button
          type="button"
          onClick={() => apply("", "")}
          className="cursor-pointer px-2 py-2 text-sm text-muted-foreground hover:text-centaur-300"
        >
          Clear
        </button>
      )}
    </form>
  );
}
