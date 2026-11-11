// Data access for the broker credential pages (/api/v1/broker_credentials).
// Query keys live under "credentials". The API never returns token material,
// write-only values, or header values: only names and has_* presence flags.
import { keepPreviousData, useMutation, useQuery, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";

import { api, type DataEnvelope, type ListEnvelope } from "@/lib/api";
import { createQueryKeys } from "@/lib/query-keys";

export const credentialKeys = createQueryKeys("credentials");

export const GRANTS = ["refresh_token", "client_credentials", "password", "preqin"] as const;
export type Grant = (typeof GRANTS)[number];

/** BrokerCredential::PREQIN_TOKEN_ENDPOINT */
export const PREQIN_TOKEN_ENDPOINT = "https://api.preqin.com/connect/token";

export type CredentialStatus = "live" | "dead" | "bootstrapping";

export interface BrokerCredential {
  id: string;
  foreign_id: string | null;
  name: string | null;
  description: string | null;
  labels: Record<string, string>;
  grant: Grant;
  token_endpoint: string;
  scopes: string[];
  client_id: string | null;
  effective_client_id: string | null;
  token_endpoint_header_names: string[];
  early_refresh_slack_seconds: number;
  early_refresh_fraction: number;
  max_refresh_interval_seconds: number;
  refresh_timeout_seconds: number;
  status: CredentialStatus;
  expires_at: string | null;
  last_refresh: string | null;
  next_attempt_at: string | null;
  dead: boolean;
  dead_reason: string | null;
  failure_count: number;
  has_client_secret: boolean;
  has_refresh_token: boolean;
  has_username: boolean;
  has_password: boolean;
  has_api_key: boolean;
  oauth_app_id: string | null;
  oauth_app: { id: string; slug: string; provider: string } | null;
  static_secret: { id: string; foreign_id: string | null; name: string | null } | null;
  provider_subject: string | null;
  provider_email: string | null;
  external_user_key: string | null;
  created_at: string;
  updated_at: string;
}

/**
 * Create/update body. Write-only fields are omitted when blank so stored values
 * are kept; a `null` header value keeps that stored header's value.
 */
// A type alias (not an interface) so it fits omitBlankSecrets' Record bound.
export type CredentialPayload = {
  foreign_id?: string | null;
  name: string;
  description: string;
  grant: Grant;
  token_endpoint: string;
  client_id: string | null;
  client_secret?: string;
  scopes: string[];
  token_endpoint_headers: Record<string, string | null> | null;
  early_refresh_fraction: string;
  early_refresh_slack_seconds: string;
  max_refresh_interval_seconds: string;
  refresh_timeout_seconds: string;
  refresh_token?: string;
  username?: string;
  password?: string;
  api_key?: string;
  labels: Record<string, string>;
};

const base = "/api/v1/broker_credentials";

/** name || foreign_id || oid (the legacy page title). */
export function credentialTitle(credential: Pick<BrokerCredential, "id" | "name" | "foreign_id">): string {
  return credential.name || credential.foreign_id || credential.id;
}

export function useCredentialList(page: number) {
  return useQuery({
    queryKey: credentialKeys.list({ page }),
    queryFn: ({ signal }) => api.get<ListEnvelope<BrokerCredential>>(base, { page }, signal),
    placeholderData: keepPreviousData,
  });
}

export function useCredential(id: string) {
  return useSuspenseQuery({
    queryKey: credentialKeys.detail(id),
    queryFn: ({ signal }) => api.get<DataEnvelope<BrokerCredential>>(`${base}/${id}`, undefined, signal),
    select: (body) => body.data,
  });
}

export function useCreateCredential() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (data: CredentialPayload) => api.post<DataEnvelope<BrokerCredential>>(base, { data }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: credentialKeys.lists() }),
  });
}

export function useUpdateCredential(id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (data: CredentialPayload) => api.patch<DataEnvelope<BrokerCredential>>(`${base}/${id}`, { data }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: credentialKeys.all }),
  });
}

export function useDeleteCredential(id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => api.delete(`${base}/${id}`),
    // Lists only: removing the detail query while its page is still mounted
    // would refetch the deleted record.
    onSuccess: () => queryClient.invalidateQueries({ queryKey: credentialKeys.lists() }),
  });
}
