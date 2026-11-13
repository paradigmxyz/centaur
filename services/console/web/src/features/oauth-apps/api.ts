import { keepPreviousData, useQuery, useSuspenseQuery, type QueryClient } from "@tanstack/react-query";

import { api, type DataEnvelope, type ListEnvelope } from "@/lib/api";
import { createQueryKeys } from "@/lib/query-keys";

export const oauthAppKeys = createQueryKeys("oauth_apps");

/** GET /api/v1/console/oauth_apps[/:id]. The client secret is never returned; see client_secret_set. */
export interface OauthApp {
  id: string;
  slug: string;
  description: string | null;
  labels: Record<string, string>;
  provider: string;
  provider_config: Record<string, string>;
  client_id: string;
  allowed_scopes: string[];
  enabled: boolean;
  always_available: boolean;
  shared: boolean;
  client_secret_set: boolean;
  redirect_uri: string;
  start_url: string;
  credentials_count: number;
  created_at: string;
  updated_at: string;
}

export interface MintedCredential {
  id: string;
  foreign_id: string | null;
  name: string | null;
  status: string;
  provider_email: string | null;
  external_user_key: string | null;
  created_at: string;
}

export interface ProviderDetails {
  provider: string;
  credential: { id: string; provider_subject: string | null; static_secret_id: string | null } | null;
  details: Record<string, unknown>;
}

export interface OauthProvider {
  key: string;
  display_name: string;
  presettable: boolean;
  config_keys: string[];
  optional_config_keys: string[];
}

export type IntegrationUsage = "shared" | "always_available" | "direct_messages";

export interface IntegrationApp {
  id: string;
  slug: string;
  provider: string;
  description: string | null;
  shared: boolean;
  always_available: boolean;
  usage: IntegrationUsage;
  start_url: string;
  connection: { status: "connected" | "needs_reconnect" } | null;
}

export interface IntegrationPreset {
  provider: string;
  display_name: string;
  description: string | null;
}

export interface Integrations {
  apps: IntegrationApp[];
  presets: IntegrationPreset[];
  ssh_key: { configured: boolean };
}

export interface OauthAppInput {
  slug: string;
  description: string;
  provider: string;
  client_id: string;
  client_secret?: string;
  provider_config: Record<string, string>;
  allowed_scopes: string[];
  enabled: boolean;
  shared?: boolean;
  always_available: boolean;
  labels: Record<string, string>;
}

const base = "/api/v1/console/oauth_apps";

/** Apps, oldest first, in pages of the API max (200). */
export function useOauthAppList(page: number) {
  return useQuery({
    queryKey: oauthAppKeys.list({ limit: 200, page }),
    queryFn: ({ signal }) => api.get<ListEnvelope<OauthApp>>(base, { limit: 200, page }, signal),
    placeholderData: keepPreviousData,
  });
}

export function useOauthApp(id: string) {
  return useSuspenseQuery({
    queryKey: oauthAppKeys.detail(id),
    queryFn: ({ signal }) => api.get<DataEnvelope<OauthApp>>(`${base}/${id}`, undefined, signal),
    select: (body) => body.data,
  });
}

export function useMintedCredentials(id: string, page: number) {
  return useQuery({
    queryKey: oauthAppKeys.sub(id, "credentials", { page }),
    queryFn: ({ signal }) => api.get<ListEnvelope<MintedCredential>>(`${base}/${id}/credentials`, { page }, signal),
    placeholderData: keepPreviousData,
  });
}

/** Live provider details for a shared app's account (Mercator wallet). */
export function useProviderDetails(id: string, enabled: boolean) {
  return useQuery({
    queryKey: oauthAppKeys.sub(id, "provider_details"),
    queryFn: ({ signal }) => api.get<DataEnvelope<ProviderDetails>>(`${base}/${id}/provider_details`, undefined, signal),
    select: (body) => body.data,
    enabled,
  });
}

export function useOauthProviders() {
  return useQuery({
    queryKey: ["oauth_apps", "providers"],
    queryFn: ({ signal }) => api.get<DataEnvelope<OauthProvider[]>>("/api/v1/console/oauth_providers", undefined, signal),
    select: (body) => body.data,
    staleTime: Infinity,
  });
}

// Create/update bodies may carry the client_secret. It lives only in this call
// and the caller's form, which clears the field afterwards.
export async function createOauthApp(queryClient: QueryClient, data: OauthAppInput) {
  const body = await api.post<DataEnvelope<OauthApp>>(base, { data });
  await queryClient.invalidateQueries({ queryKey: oauthAppKeys.lists() });
  return body;
}

export async function updateOauthApp(queryClient: QueryClient, id: string, data: Partial<OauthAppInput>) {
  const body = await api.patch<DataEnvelope<OauthApp>>(`${base}/${id}`, { data });
  await queryClient.invalidateQueries({ queryKey: oauthAppKeys.all });
  return body;
}

export const integrationKeys = createQueryKeys("integrations");

export function useIntegrations() {
  return useQuery({
    queryKey: integrationKeys.all,
    queryFn: ({ signal }) => api.get<DataEnvelope<Integrations>>("/api/v1/console/integrations", undefined, signal),
    select: (body) => body.data,
  });
}

/** POST /api/v1/console/oauth_apps/presets/:provider -> the consent start URL. */
export async function setupPreset(queryClient: QueryClient, provider: string) {
  try {
    return await api.post<DataEnvelope<{ id: string; slug: string; start_url: string }>>(
      `${base}/presets/${encodeURIComponent(provider)}`,
    );
  } finally {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: integrationKeys.all }),
      queryClient.invalidateQueries({ queryKey: oauthAppKeys.lists() }),
    ]);
  }
}
