// API client secrets are never cached: the create/rotate responses go straight
// to the caller (which keeps the secret in route-scoped component state);
// detail queries are refreshed by invalidation, never seeded from them.
import { useQuery, useSuspenseQuery, type QueryClient } from "@tanstack/react-query";

import { api, type DataEnvelope } from "@/lib/api";
import { createQueryKeys } from "@/lib/query-keys";

export const adminKeys = createQueryKeys("admin");

interface CountedList<T> {
  data: T[];
  meta: { total: number };
}

export type UserStatus = "active" | "pending" | "disabled";

export interface ConsoleUser {
  id: string;
  email: string;
  name: string | null;
  status: UserStatus;
  admin: boolean;
  current: boolean;
  identity_providers: string[];
  approved_at: string | null;
  approved_by: { id: string; email: string } | null;
  created_at: string;
  updated_at: string;
}

export type UserAction = "approve" | "disable" | "promote" | "demote";

const usersBase = "/api/v1/users";

export function useUsers() {
  return useQuery({
    queryKey: adminKeys.list({ resource: "users" }),
    queryFn: ({ signal }) => api.get<CountedList<ConsoleUser>>(usersBase, undefined, signal),
    select: (body) => body.data,
  });
}

export async function runUserAction(queryClient: QueryClient, user: ConsoleUser, action: UserAction) {
  try {
    return await api.post<DataEnvelope<ConsoleUser>>(`${usersBase}/${user.id}/${action}`);
  } finally {
    await queryClient.invalidateQueries({ queryKey: adminKeys.list({ resource: "users" }) });
  }
}

export const API_CLIENT_SCOPES = ["sessions:read", "sessions:write"] as const;

export interface ApiClientPrincipalLink {
  id: string;
  default: boolean;
  principal: { id: string; foreign_id: string | null; name: string | null; kind: string };
}

export interface ApiClient {
  id: string;
  name: string;
  scopes: string[];
  disabled: boolean;
  disabled_at: string | null;
  principals_count: number;
  principals: ApiClientPrincipalLink[];
  created_at: string;
  updated_at: string;
}

/** The create / rotate_secret response: the only place the secret appears. */
export type IssuedApiClient = ApiClient & { secret: string };

export interface ApiClientInput {
  name: string;
  scopes: string[];
  principal?: string;
}

const clientsBase = "/api/v1/api_clients";
const clientsListKey = adminKeys.list({ resource: "api_clients" });
const clientKey = (id: string) => adminKeys.detail(`api_client:${id}`);

export function useApiClients() {
  return useQuery({
    queryKey: clientsListKey,
    queryFn: ({ signal }) => api.get<CountedList<ApiClient>>(clientsBase, undefined, signal),
    select: (body) => body.data,
  });
}

export function useApiClient(id: string) {
  return useSuspenseQuery({
    queryKey: clientKey(id),
    queryFn: ({ signal }) => api.get<DataEnvelope<ApiClient>>(`${clientsBase}/${id}`, undefined, signal),
    select: (body) => body.data,
  });
}

/** The response carries the plaintext secret; never cache it. */
export async function createApiClient(queryClient: QueryClient, data: ApiClientInput) {
  const body = await api.post<DataEnvelope<IssuedApiClient>>(clientsBase, { data });
  await queryClient.invalidateQueries({ queryKey: clientsListKey });
  return body;
}

export async function updateApiClient(queryClient: QueryClient, id: string, data: Omit<ApiClientInput, "principal">) {
  const body = await api.patch<DataEnvelope<ApiClient>>(`${clientsBase}/${id}`, { data });
  queryClient.setQueryData(clientKey(id), body);
  await queryClient.invalidateQueries({ queryKey: clientsListKey });
  return body;
}

const clientPath = (id: string) => `${clientsBase}/${id}`;

const refreshApiClient = (queryClient: QueryClient, id: string) =>
  Promise.all([
    queryClient.invalidateQueries({ queryKey: clientKey(id) }),
    queryClient.invalidateQueries({ queryKey: clientsListKey }),
  ]);

/** The response carries the new plaintext secret; never cache it. */
export async function rotateApiClientSecret(queryClient: QueryClient, id: string) {
  const body = await api.post<DataEnvelope<IssuedApiClient>>(`${clientPath(id)}/rotate_secret`);
  await refreshApiClient(queryClient, id);
  return body;
}

export async function setApiClientEnabled(queryClient: QueryClient, id: string, enabled: boolean) {
  await api.post(`${clientPath(id)}/${enabled ? "enable" : "disable"}`);
  await refreshApiClient(queryClient, id);
}

export async function deleteApiClient(queryClient: QueryClient, id: string) {
  await api.delete(clientPath(id));
  // List only: dropping the detail query while its page is mounted would refetch it.
  await queryClient.invalidateQueries({ queryKey: clientsListKey });
}

export async function attachApiClientPrincipal(queryClient: QueryClient, id: string, principal: string) {
  const body = await api.post<DataEnvelope<ApiClientPrincipalLink>>(`${clientPath(id)}/principals`, { data: { principal } });
  await refreshApiClient(queryClient, id);
  return body;
}

export async function detachApiClientPrincipal(queryClient: QueryClient, id: string, linkId: string) {
  await api.delete(`${clientPath(id)}/principals/${linkId}`);
  await refreshApiClient(queryClient, id);
}

export async function makeApiClientPrincipalDefault(queryClient: QueryClient, id: string, linkId: string) {
  await api.post(`${clientPath(id)}/principals/${linkId}/default`);
  await refreshApiClient(queryClient, id);
}

export type SandboxRepoCache = "none" | "public" | "all";

export interface SettingsRole {
  id: string;
  foreign_id: string | null;
  name: string | null;
  assign_by_default: boolean;
}

export interface SystemSettings {
  default_sandbox_repo_cache: SandboxRepoCache;
  default_sandbox_observability_enabled: boolean;
  default_sandbox_sessions_read_enabled: boolean;
  default_sandbox_workflows_read_enabled: boolean;
  default_sandbox_workflows_write_enabled: boolean;
  default_role_ids: string[];
  roles: SettingsRole[];
  updated_at: string;
}

export type SystemSettingsInput = Omit<SystemSettings, "roles" | "updated_at">;

const settingsBase = "/api/v1/system_settings";
const settingsKey = adminKeys.detail("system_settings");
const slackTeamsKey = adminKeys.sub("system_settings", "slack_external_teams");

export function useSystemSettings() {
  return useSuspenseQuery({
    queryKey: settingsKey,
    queryFn: ({ signal }) => api.get<DataEnvelope<SystemSettings>>(settingsBase, undefined, signal),
    select: (body) => body.data,
  });
}

export async function updateSystemSettings(queryClient: QueryClient, data: SystemSettingsInput) {
  const body = await api.patch<DataEnvelope<SystemSettings>>(settingsBase, { data });
  queryClient.setQueryData(settingsKey, body);
  return body;
}

export function useSlackExternalTeams() {
  return useQuery({
    queryKey: slackTeamsKey,
    queryFn: ({ signal }) =>
      api.get<DataEnvelope<{ slack_external_team_ids: string[] }>>(`${settingsBase}/slack_external_teams`, undefined, signal),
    select: (body) => body.data.slack_external_team_ids,
    retry: false,
  });
}

export async function replaceSlackExternalTeams(queryClient: QueryClient, text: string) {
  const body = await api.put<DataEnvelope<{ slack_external_team_ids: string[] }>>(`${settingsBase}/slack_external_teams`, {
    data: { slack_external_team_ids: text },
  });
  queryClient.setQueryData(slackTeamsKey, body);
  return body;
}
