// Role changes also refresh cached principal pages, which show role names,
// inherited Slack permissions, and effective grants.
import { keepPreviousData, useQuery, useSuspenseQuery, type QueryClient } from "@tanstack/react-query";

import type { SlackChannelPermissionRow } from "@/components/slack-channel-permissions-editor";
import { api, type DataEnvelope, type ListEnvelope } from "@/lib/api";
import { createQueryKeys, sharedKeys } from "@/lib/query-keys";
import { grantKey, type GrantableSummary } from "@/lib/secret-kinds";

export const roleKeys = createQueryKeys("roles");

/** Root of the shared secrets-list keys (grant picker, assignable count). */
const SECRETS_ROOT = sharedKeys.secrets().slice(0, 1);

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

/** Roles are few: one page of the API maximum. */
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

/** How many secrets can still be granted to the role (all kinds, excluding existing grants). */
export function useAssignableSecretCount(id: string) {
  const params = { exclude_grantee: `role:${id}`, limit: 1 };
  return useQuery({
    queryKey: sharedKeys.secrets(params),
    queryFn: ({ signal }) => api.get<ListEnvelope<GrantableSummary>>("/api/v1/secrets", params, signal),
    select: (body) => body.meta.total,
  });
}

/** Refreshes one role and cached principal pages (which embed role data). */
function refreshRole(queryClient: QueryClient, id: string) {
  return Promise.all([
    queryClient.invalidateQueries({ queryKey: roleKeys.detail(id) }),
    queryClient.invalidateQueries({ queryKey: PRINCIPALS_ROOT }),
  ]);
}

/** Grant changes also change which secrets the picker may offer. */
function refreshRoleGrants(queryClient: QueryClient, id: string) {
  return Promise.all([refreshRole(queryClient, id), queryClient.invalidateQueries({ queryKey: SECRETS_ROOT })]);
}

export async function createRole(queryClient: QueryClient, data: RoleFormData) {
  const body = await api.post<DataEnvelope<Role>>(base, { data });
  await queryClient.invalidateQueries({ queryKey: roleKeys.lists() });
  return body;
}

export async function updateRole(queryClient: QueryClient, id: string, data: RoleFormData) {
  const body = await api.patch<DataEnvelope<Role>>(`${base}/${id}`, { data });
  await Promise.all([refreshRole(queryClient, id), queryClient.invalidateQueries({ queryKey: roleKeys.lists() })]);
  return body;
}

export async function replaceRoleSlackPermissions(queryClient: QueryClient, id: string, rows: SlackChannelPermissionRow[]) {
  await api.put(`${base}/${id}/slack_channel_permissions`, { data: { slack_channel_permissions: rows } });
  await refreshRole(queryClient, id);
}

export async function deleteRoleSlackPermission(queryClient: QueryClient, id: string, channelId: string) {
  await api.delete(`${base}/${id}/slack_channel_permissions/${encodeURIComponent(channelId)}`);
  await refreshRole(queryClient, id);
}

export async function grantRoleSecret(queryClient: QueryClient, id: string, secret: Pick<GrantableSummary, "kind" | "id">) {
  await api.post("/api/v1/grants", { data: { role_id: id, [grantKey(secret.kind)]: secret.id } });
  await refreshRoleGrants(queryClient, id);
}

export async function revokeRoleGrant(queryClient: QueryClient, id: string, grantId: string) {
  await api.delete(`/api/v1/grants/${grantId}`);
  await refreshRoleGrants(queryClient, id);
}
