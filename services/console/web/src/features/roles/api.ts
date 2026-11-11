// Data access for the role pages. Query keys live under "roles" (see
// lib/query-keys.ts); mutations invalidate the narrowest prefix. Role changes
// also refresh cached principal pages, which show role names, inherited Slack
// permissions, and effective grants.
import { keepPreviousData, useMutation, useQuery, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";

import type { SlackChannelPermissionRow } from "@/components/slack-channel-permissions-editor";
import { api, type DataEnvelope, type ListEnvelope } from "@/lib/api";
import { createQueryKeys } from "@/lib/query-keys";
import { grantKey, type GrantableSummary } from "@/lib/secret-kinds";

export const roleKeys = createQueryKeys("roles");

/** Root key of the principals slice, whose pages embed role data. */
const PRINCIPALS_ROOT = ["principals"] as const;

export interface Role {
  id: string;
  foreign_id: string | null;
  name: string | null;
  labels: Record<string, string>;
  slack_channel_permissions: SlackChannelPermissionRow[];
  created_at: string;
  updated_at: string;
}

export interface RoleGrant {
  id: string;
  role_id: string;
  grantable: GrantableSummary;
  created_at: string;
}

export interface RoleFormData {
  foreign_id?: string | null;
  name: string;
  labels: Record<string, string>;
}

const base = "/api/v1/roles";

/** Roles are few and the legacy page was unpaginated: one page of the API maximum. */
export const ROLE_PAGE_LIMIT = 200;

export function useRoleList(page: number) {
  return useQuery({
    queryKey: roleKeys.list(page === 1 ? { limit: ROLE_PAGE_LIMIT } : { limit: ROLE_PAGE_LIMIT, page }),
    queryFn: ({ signal }) => api.get<ListEnvelope<Role>>(base, { limit: ROLE_PAGE_LIMIT, page }, signal),
    placeholderData: keepPreviousData,
  });
}

export function useRole(id: string) {
  return useSuspenseQuery({
    queryKey: roleKeys.detail(id),
    queryFn: ({ signal }) => api.get<DataEnvelope<Role>>(`${base}/${id}`, undefined, signal),
    select: (body) => body.data,
  });
}

export function useRoleGrants(id: string, page: number) {
  return useQuery({
    queryKey: roleKeys.sub(id, "grants", { page }),
    queryFn: ({ signal }) =>
      api.get<ListEnvelope<RoleGrant>>(`${base}/${id}/grants`, { limit: ROLE_PAGE_LIMIT, page }, signal),
    placeholderData: keepPreviousData,
  });
}

/** Mutations for one role; each refreshes that role and cached principal pages. */
export function useRoleMutations(id: string) {
  const queryClient = useQueryClient();
  const refresh = () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: roleKeys.detail(id) }),
      queryClient.invalidateQueries({ queryKey: PRINCIPALS_ROOT }),
    ]);

  return {
    update: useMutation({
      mutationFn: (data: RoleFormData) => api.patch<DataEnvelope<Role>>(`${base}/${id}`, { data }),
      onSuccess: () =>
        Promise.all([refresh(), queryClient.invalidateQueries({ queryKey: roleKeys.lists() })]),
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
    grantSecret: useMutation({
      mutationFn: (secret: Pick<GrantableSummary, "kind" | "id">) =>
        api.post("/api/v1/grants", { data: { role_id: id, [grantKey(secret.kind)]: secret.id } }),
      onSuccess: refresh,
    }),
    revokeGrant: useMutation({
      mutationFn: (grantId: string) => api.delete(`/api/v1/grants/${grantId}`),
      onSuccess: refresh,
    }),
  };
}

export function useCreateRole() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (data: RoleFormData) => api.post<DataEnvelope<Role>>(base, { data }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: roleKeys.lists() }),
  });
}
