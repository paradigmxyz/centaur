// Data access for the secrets pages. Query keys live under "secrets" (shared
// with the grant pickers' sharedKeys.secrets lists, so invalidating the lists
// refreshes them too); writes invalidate the narrowest prefix.
import { keepPreviousData, useQuery, useSuspenseQuery, type QueryClient } from "@tanstack/react-query";

import { api, type DataEnvelope, type ListEnvelope } from "@/lib/api";
import { createQueryKeys } from "@/lib/query-keys";
import type { GrantableSummary, SecretKind } from "@/lib/secret-kinds";

export const secretKeys = createQueryKeys("secrets");

/** The secret kinds the console creates and edits (SECRET_KINDS form: true). */
export type FormKind = "static" | "pg_dsn" | "gcp_auth" | "gcp_id_token";

export const FORM_KINDS: readonly FormKind[] = ["static", "gcp_auth", "gcp_id_token", "pg_dsn"];

export function isFormKind(kind: string): kind is FormKind {
  return (FORM_KINDS as readonly string[]).includes(kind);
}

/** The per-kind /api/v1 resource behind each form kind. */
export const KIND_RESOURCE: Record<FormKind, string> = {
  static: "/api/v1/static_secrets",
  pg_dsn: "/api/v1/pg_dsn_secrets",
  gcp_auth: "/api/v1/gcp_auth_secrets",
  gcp_id_token: "/api/v1/gcp_id_token_secrets",
};

export interface ManagedBy {
  broker_credential_id: string;
  oauth_app_id: string | null;
  oauth_app_slug: string | null;
}

/** A row of GET /api/v1/secrets. */
export interface SecretListItem extends GrantableSummary {
  grant_key: string;
  description: string | null;
  labels: Record<string, string>;
  source_types: string[];
  managed_by: ManagedBy | null;
  created_at: string;
  updated_at: string;
}

export interface SourceSegment {
  role: string | null;
  type: string;
  type_label: string;
  ref: string | null;
}

export interface RequestRule {
  host: string | null;
  cidr: string | null;
  position: number;
  http_methods: string[];
  paths: string[];
}

/** GET /api/v1/secrets/:kind/:id. */
export interface SecretDetail extends SecretListItem {
  editable: boolean;
  injection: string | null;
  source_segments: SourceSegment[];
  configuration: { label: string; value: string }[];
  signed_headers: { name: string; value: string }[] | null;
  rules: RequestRule[] | null;
}

export interface RoleSummary {
  id: string;
  foreign_id: string | null;
  name: string | null;
}

export interface RoleGrant {
  id: string;
  role_id: string;
  role: RoleSummary;
  priority: number;
  created_at: string;
}

export interface RoleGrantsEnvelope {
  data: RoleGrant[];
  assignable_roles: RoleSummary[];
}

/** A source as the per-kind resources return it (never the inline value). */
export interface SourcePayload {
  source_type: string;
  config: Record<string, unknown>;
}

/** The per-kind record (GET /api/v1/<kind>_secrets/:id), as the edit form needs it. */
export interface KindRecord {
  id: string;
  foreign_id: string | null;
  name: string | null;
  description: string | null;
  labels: Record<string, string>;
  // static
  kind?: string;
  inject_config?: Record<string, unknown> | null;
  replace_config?: Record<string, unknown> | null;
  source?: SourcePayload | null;
  rules?: RequestRule[];
  // pg_dsn
  database?: string | null;
  role?: string | null;
  settings?: { name: string; value?: string; value_from?: Record<string, string> }[];
  dsn?: SourcePayload | null;
  // gcp_auth / gcp_id_token
  credentials_provider?: Record<string, unknown> | null;
  subject?: string | null;
  scopes?: string[];
  keyfile?: SourcePayload | null;
  audience?: string | null;
  header?: string | null;
  created_at: string;
  updated_at: string;
}

export const SECRETS_PER_PAGE = 50;

/** Page title of a secret: name, else foreign_id, else oid. */
export function secretTitle(secret: { name: string | null; foreign_id: string | null; id: string }) {
  return secret.name || secret.foreign_id || secret.id;
}

export function useSecretList(params: { q: string; kind: string; page: number }) {
  return useQuery({
    queryKey: secretKeys.list(params),
    queryFn: ({ signal }) =>
      api.get<ListEnvelope<SecretListItem>>(
        "/api/v1/secrets",
        { q: params.q, kind: params.kind, page: params.page, limit: SECRETS_PER_PAGE },
        signal,
      ),
    placeholderData: keepPreviousData,
  });
}

/** Unfiltered count of every kind for the "N secrets total." subtitle. */
export function useSecretTotal() {
  return useQuery({
    queryKey: secretKeys.list({ total: true }),
    queryFn: ({ signal }) => api.get<ListEnvelope<SecretListItem>>("/api/v1/secrets", { limit: 1 }, signal),
    select: (body) => body.meta.total,
  });
}

const detailPath = (kind: string, id: string) =>
  `/api/v1/secrets/${encodeURIComponent(kind)}/${encodeURIComponent(id)}`;

export function useSecretDetail(kind: string, id: string) {
  return useSuspenseQuery({
    queryKey: secretKeys.detail(`${kind}:${id}`),
    queryFn: ({ signal }) => api.get<DataEnvelope<SecretDetail>>(detailPath(kind, id), undefined, signal),
    select: (body) => body.data,
  });
}

export function useKindRecord(kind: FormKind, id: string) {
  return useSuspenseQuery({
    queryKey: secretKeys.sub(`${kind}:${id}`, "record"),
    queryFn: ({ signal }) => api.get<DataEnvelope<KindRecord>>(`${KIND_RESOURCE[kind]}/${id}`, undefined, signal),
    select: (body) => body.data,
  });
}

export function useRoleGrants(kind: string, id: string) {
  return useQuery({
    queryKey: secretKeys.sub(`${kind}:${id}`, "role_grants"),
    queryFn: ({ signal }) => api.get<RoleGrantsEnvelope>(`${detailPath(kind, id)}/role_grants`, undefined, signal),
  });
}

const refreshRoleGrants = (queryClient: QueryClient, kind: string, id: string) =>
  queryClient.invalidateQueries({ queryKey: secretKeys.sub(`${kind}:${id}`, "role_grants") });

export async function assignSecretRole(queryClient: QueryClient, kind: string, id: string, roleId: string) {
  const body = await api.post<DataEnvelope<RoleGrant>>(`${detailPath(kind, id)}/role_grants`, { data: { role_id: roleId } });
  await refreshRoleGrants(queryClient, kind, id);
  return body;
}

export async function unassignSecretRole(queryClient: QueryClient, kind: string, id: string, grantId: string) {
  await api.delete(`${detailPath(kind, id)}/role_grants/${encodeURIComponent(grantId)}`);
  await refreshRoleGrants(queryClient, kind, id);
}

/**
 * Create (no id) or PATCH (id) one form-kind secret. The body may carry an
 * inline source value: it lives only in this call and the caller's form,
 * which clears the field afterwards.
 */
export async function saveSecret(queryClient: QueryClient, kind: FormKind, id: string | undefined, data: Record<string, unknown>) {
  const body = id
    ? await api.patch<DataEnvelope<KindRecord>>(`${KIND_RESOURCE[kind]}/${id}`, { data })
    : await api.post<DataEnvelope<KindRecord>>(KIND_RESOURCE[kind], { data });
  await Promise.all([
    queryClient.invalidateQueries({ queryKey: secretKeys.lists() }),
    queryClient.invalidateQueries({ queryKey: secretKeys.detail(`${kind}:${body.data.id}`) }),
  ]);
  return body;
}

export async function deleteSecret(queryClient: QueryClient, kind: FormKind, id: string) {
  await api.delete(`${KIND_RESOURCE[kind]}/${id}`);
  // Lists only: removing the detail query while its page is still mounted
  // would refetch the deleted record.
  await queryClient.invalidateQueries({ queryKey: secretKeys.lists() });
}

export type { SecretKind };
