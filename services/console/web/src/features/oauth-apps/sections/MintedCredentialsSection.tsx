import { AppLink } from "@/components/app-link";
import { credentialStatusTone, StatusBadge } from "@/components/badges";
import { DataTable, type Column } from "@/components/data-table";
import { EmptyState } from "@/components/empty-state";
import { SectionHeading } from "@/components/page-header";
import { usePageParam } from "@/components/pagination";

import { useMintedCredentials, type MintedCredential } from "../api";

const columns: Column<MintedCredential>[] = [
  {
    id: "credential",
    header: "Credential",
    className: "whitespace-nowrap",
    cell: (credential) => (
      <AppLink
        to={`/console/credentials/${credential.id}`}
        className="text-centaur-400 hover:text-centaur-300 hover:underline light:text-primary"
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
      {list.data && list.data.meta.total === 0 ? (
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
            list.data && list.data.meta.total_pages > 1
              ? { meta: list.data.meta, unit: "credential", onPageChange: setPage }
              : undefined
          }
        />
      )}
    </section>
  );
}
