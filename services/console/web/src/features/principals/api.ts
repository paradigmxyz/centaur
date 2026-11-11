// Data access for the principals pages. Query keys live under "principals"
// (see lib/query-keys.ts); mutations invalidate the narrowest prefix.
import { keepPreviousData, useMutation, useQuery, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";

import type { SlackChannelPermissionRow } from "@/components/slack-channel-permissions-editor";
import { api, type DataEnvelope, type ListEnvelope } from "@/lib/api";
import { createQueryKeys } from "@/lib/query-keys";
import { grantKey, type GrantableSummary, type SecretKind } from "@/lib/secret-kinds";

export const principalKeys = createQueryKeys("principals");

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

/** All roles, for the assign-role select (roles are few; 200 is the API max). */
export function useAllRoles() {
  return useQuery({
    queryKey: ["roles", "list", { limit: 200 }],
    queryFn: ({ signal }) => api.get<ListEnvelope<RoleSummary>>("/api/v1/roles", { limit: 200 }, signal),
    select: (body) => body.data,
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

/** Mutations for one principal; each refreshes that principal's queries. */
export function usePrincipalMutations(id: string) {
  const queryClient = useQueryClient();
  const refresh = () => queryClient.invalidateQueries({ queryKey: principalKeys.detail(id) });
  const refreshAll = () => queryClient.invalidateQueries({ queryKey: principalKeys.all });

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
      onSuccess: refreshAll,
    }),
    unassignRole: useMutation({
      mutationFn: (roleId: string) => api.delete(`${base}/${id}/roles/${roleId}`),
      onSuccess: refreshAll,
    }),
    grantSecret: useMutation({
      mutationFn: (secret: Pick<GrantableSummary, "kind" | "id">) =>
        api.post("/api/v1/grants", { data: { principal_id: id, [grantKey(secret.kind)]: secret.id } }),
      onSuccess: refresh,
    }),
    revokeGrant: useMutation({
      mutationFn: (grantId: string) => api.delete(`/api/v1/grants/${grantId}`),
      onSuccess: refresh,
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
