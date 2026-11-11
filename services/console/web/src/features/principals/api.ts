// Data access for the principals pages. Query keys live under "principals"
// (see lib/query-keys.ts); mutations invalidate the narrowest prefix.
import { keepPreviousData, useMutation, useQuery, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";

import type { SlackChannelPermissionRow } from "@/components/slack-channel-permissions-editor";
import { api, type DataEnvelope, type ListEnvelope } from "@/lib/api";
import { createQueryKeys, sharedKeys } from "@/lib/query-keys";
import { grantKey, type GrantableSummary, type SecretKind } from "@/lib/secret-kinds";

export const principalKeys = createQueryKeys("principals");

/** Root of the shared secrets-list keys (grant picker, assignable count). */
const SECRETS_ROOT = sharedKeys.secrets().slice(0, 1);

export interface RoleSummary {
  id: string;
  foreign_id: string | null;
  name: string | null;
}

export type SandboxRepoCache = "none" | "public" | "all";

export interface Principal {
  id: string;
  foreign_id: string | null;
  name: string | null;
  kind: string;
  slack_user_id: string | null;
  labels: Record<string, string>;
  roles: RoleSummary[];
  slack_channel_permissions: SlackChannelPermissionRow[];
  inherited_slack_channel_permissions: SlackChannelPermissionRow[];
  effective_slack_channel_permissions: SlackChannelPermissionRow[];
  sandbox_repo_cache: SandboxRepoCache;
  sandbox_observability_enabled: boolean;
  sandbox_sessions_read_enabled: boolean;
  sandbox_workflows_read_enabled: boolean;
  sandbox_workflows_write_enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface DirectGrant {
  id: string;
  principal_id: string;
  grantable: GrantableSummary;
  created_at: string;
}

export interface EffectiveGrant {
  id: string;
  kind: SecretKind;
  foreign_id: string | null;
  name: string | null;
  direct: boolean;
  roles: RoleSummary[];
}

export type SandboxAccess = Pick<
  Principal,
  | "sandbox_repo_cache"
  | "sandbox_observability_enabled"
  | "sandbox_sessions_read_enabled"
  | "sandbox_workflows_read_enabled"
  | "sandbox_workflows_write_enabled"
>;

const base = "/api/v1/principals";

export function usePrincipalList(params: { q: string; page: number }) {
  return useQuery({
    queryKey: principalKeys.list(params),
    queryFn: ({ signal }) =>
      api.get<ListEnvelope<Principal>>(base, { view: "console", sort: "name", q: params.q, page: params.page }, signal),
    placeholderData: keepPreviousData,
  });
}

/** Unfiltered principal count for the "N principals total." subtitle. */
export function usePrincipalTotal() {
  return useQuery({
    queryKey: principalKeys.list({ total: true }),
    queryFn: ({ signal }) => api.get<ListEnvelope<Principal>>(base, { limit: 1 }, signal),
    select: (body) => body.meta.total,
  });
}

export function usePrincipal(id: string) {
  return useSuspenseQuery({
    queryKey: principalKeys.detail(id),
    queryFn: ({ signal }) => api.get<DataEnvelope<Principal>>(`${base}/${id}`, { view: "console" }, signal),
    select: (body) => body.data,
  });
}

export function usePrincipalRoles(id: string) {
  return useQuery({
    queryKey: principalKeys.sub(id, "roles"),
    queryFn: ({ signal }) => api.get<DataEnvelope<RoleSummary[]>>(`${base}/${id}/roles`, undefined, signal),
    select: (body) => body.data,
  });
}

/** Root of the roles-list keys the assign-role picker reads (owned by the roles slice). */
const ROLE_LISTS = ["roles", "list"] as const;

/** The API maximum page size; the picker searches server-side beyond it. */
export const ROLE_PICKER_LIMIT = 200;

function assignableRolesQuery(id: string, params: { q?: string; limit: number }) {
  const query = { exclude_principal: id, limit: params.limit, ...(params.q ? { q: params.q } : {}) };
  return {
    queryKey: [...ROLE_LISTS, query] as const,
    queryFn: ({ signal }: { signal: AbortSignal }) =>
      api.get<ListEnvelope<RoleSummary>>("/api/v1/roles", query, signal),
  };
}

/**
 * Roles not yet assigned to the principal (the server excludes them across
 * all pages): `total` decides whether the assign form shows, and `first` is
 * the picker's default, as the legacy select preselected its first option.
 */
export function useAssignableRoleSummary(id: string) {
  return useQuery({
    ...assignableRolesQuery(id, { limit: 1 }),
    select: (body) => ({ total: body.meta.total, first: body.data[0] ?? null }),
  });
}

/** One search page of assignable roles for the picker, fetched while it is open. */
export function useAssignableRoles(id: string, q: string, enabled: boolean) {
  return useQuery({
    ...assignableRolesQuery(id, { q, limit: ROLE_PICKER_LIMIT }),
    enabled,
    placeholderData: keepPreviousData,
  });
}

export function useDirectGrants(id: string, page: number) {
  return useQuery({
    queryKey: principalKeys.sub(id, "grants", { page }),
    queryFn: ({ signal }) => api.get<ListEnvelope<DirectGrant>>(`${base}/${id}/grants`, { page }, signal),
    placeholderData: keepPreviousData,
  });
}

export function useEffectiveGrants(id: string, page: number) {
  return useQuery({
    queryKey: principalKeys.sub(id, "effective_grants", { page }),
    queryFn: ({ signal }) => api.get<ListEnvelope<EffectiveGrant>>(`${base}/${id}/effective_grants`, { page }, signal),
    placeholderData: keepPreviousData,
  });
}

/**
 * How many secrets can still be granted directly (all kinds, excluding
 * existing direct grants; role-inherited secrets stay grantable).
 */
export function useAssignableSecretCount(id: string) {
  const params = { exclude_grantee: `principal:${id}`, limit: 1 };
  return useQuery({
    queryKey: sharedKeys.secrets(params),
    queryFn: ({ signal }) => api.get<ListEnvelope<GrantableSummary>>("/api/v1/secrets", params, signal),
    select: (body) => body.meta.total,
  });
}

/** Mutations for one principal; each refreshes that principal's queries. */
export function usePrincipalMutations(id: string) {
  const queryClient = useQueryClient();
  const refresh = () => queryClient.invalidateQueries({ queryKey: principalKeys.detail(id) });
  const refreshAll = () => queryClient.invalidateQueries({ queryKey: principalKeys.all });
  // Role assignments also change which roles the picker may offer.
  const refreshRoles = () =>
    Promise.all([refreshAll(), queryClient.invalidateQueries({ queryKey: ROLE_LISTS })]);
  // Grant changes also change which secrets the picker may offer.
  const refreshGrants = () =>
    Promise.all([refresh(), queryClient.invalidateQueries({ queryKey: SECRETS_ROOT })]);

  return {
    updateSandboxAccess: useMutation({
      mutationFn: (access: SandboxAccess) => api.patch(`${base}/${id}`, { data: access }),
      onSuccess: refresh,
    }),
    replaceSlackPermissions: useMutation({
      mutationFn: (rows: SlackChannelPermissionRow[]) =>
        api.put(`${base}/${id}/slack_channel_permissions`, { data: { slack_channel_permissions: rows } }),
      onSuccess: refresh,
    }),
    deleteSlackPermission: useMutation({
      mutationFn: (channelId: string) =>
        api.delete(`${base}/${id}/slack_channel_permissions/${encodeURIComponent(channelId)}`),
      onSuccess: refresh,
    }),
    assignRole: useMutation({
      mutationFn: (roleId: string) => api.post(`${base}/${id}/roles`, { data: { role_id: roleId } }),
      onSuccess: refreshRoles,
    }),
    unassignRole: useMutation({
      mutationFn: (roleId: string) => api.delete(`${base}/${id}/roles/${roleId}`),
      onSuccess: refreshRoles,
    }),
    grantSecret: useMutation({
      mutationFn: (secret: Pick<GrantableSummary, "kind" | "id">) =>
        api.post("/api/v1/grants", { data: { principal_id: id, [grantKey(secret.kind)]: secret.id } }),
      onSuccess: refreshGrants,
    }),
    revokeGrant: useMutation({
      mutationFn: (grantId: string) => api.delete(`/api/v1/grants/${grantId}`),
      onSuccess: refreshGrants,
    }),
    destroy: useMutation({
      mutationFn: () => api.delete(`${base}/${id}`),
      // Lists only: removing the detail query while its page is still mounted
      // would refetch the deleted record.
      onSuccess: () => queryClient.invalidateQueries({ queryKey: principalKeys.lists() }),
    }),
  };
}

export interface NewPrincipal {
  foreign_id: string | null;
  name: string;
  labels: Record<string, string>;
}

export function useCreatePrincipal() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (data: NewPrincipal) => api.post<DataEnvelope<Principal>>(base, { data }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: principalKeys.lists() }),
  });
}
