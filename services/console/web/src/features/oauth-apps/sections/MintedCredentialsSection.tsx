import { ErrorAlert } from "@/components/alerts";
import { AppLink } from "@/components/app-link";
import { credentialStatusTone, StatusBadge } from "@/components/badges";
import { DataTable, type Column } from "@/components/data-table";
import { EmptyState } from "@/components/empty-state";
import { SectionHeading } from "@/components/page-header";
import { usePageParam } from "@/components/pagination";
import { errorMessage } from "@/lib/api";

import { useMintedCredentials, type MintedCredential } from "../api";

const columns: Column<MintedCredential>[] = [
  {
    id: "credential",
    header: "Credential",
    className: "whitespace-nowrap",
    cell: (credential) => (
      <AppLink
        to={`/console/credentials/${credential.id}`}
        className="text-primary hover:underline"
      >
        {credential.foreign_id || credential.id}
      </AppLink>
    ),
  },
  {
    id: "status",
    header: "Status",
    className: "whitespace-nowrap",
    cell: (credential) => <StatusBadge tone={credentialStatusTone(credential.status)}>{credential.status}</StatusBadge>,
  },
  {
    id: "email",
    header: "Email",
    className: "whitespace-nowrap text-muted-foreground",
    cell: (credential) => credential.provider_email || "—",
  },
  {
    id: "user_key",
    header: "User key",
    className: "whitespace-nowrap text-muted-foreground",
    cell: (credential) => credential.external_user_key || "—",
  },
];

/** The credentials this app minted (legacy "Minted Credentials" table). */
export function MintedCredentialsSection({ appId }: { appId: string }) {
  const [page, setPage] = usePageParam("credentials_page");
  const list = useMintedCredentials(appId, page);

  return (
    <section className="mb-8">
      <SectionHeading className="mb-3">Minted Credentials</SectionHeading>
      {list.isError && <ErrorAlert>Could not load minted credentials: {errorMessage(list.error)}</ErrorAlert>}
      {/* A failed request never shows the empty state; stale non-empty rows stay after a failed refetch. */}
      {list.isError && (list.data?.data.length ?? 0) === 0 ? null : list.data && list.data.meta.total === 0 ? (
        <EmptyState>No credentials minted yet.</EmptyState>
      ) : (
        <DataTable
          caption="Minted credentials"
          columns={columns}
          rows={list.data?.data}
          loading={list.isLoading}
          rowKey={(credential) => credential.id}
          empty="No credentials minted yet."
          pagination={
            list.data
              ? { meta: list.data.meta, unit: "credential", onPageChange: setPage, hideSinglePage: true }
              : undefined
          }
        />
      )}
    </section>
  );
}
