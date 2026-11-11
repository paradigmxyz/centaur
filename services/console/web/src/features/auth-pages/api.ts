// Data access for the auth pages (login, pending, OAuth result, MCP consent).
//
// These pages render outside the app shell and never call useSession():
// - /login and /oauth/result read the public GET /api/v1/auth/config, which
//   also hands off (once) the flash left by a redirect-based flow.
// - /pending probes GET /api/v1/me itself, because pending and signed-out
//   browsers must not be bounced by the shell's redirects.
// - /mcp/oauth/consent reads a server-issued consent ticket and posts the
//   decision as a classic form; the server answers with the redirect to the
//   client, so no client URL or authorization code passes through JavaScript
//   or the TanStack caches.
import { useMutation, useQuery } from "@tanstack/react-query";

import type { Me } from "@/app/session";
import { ApiError, apiRequest, setCsrfToken, type DataEnvelope } from "@/lib/api";
import { createQueryKeys } from "@/lib/query-keys";

export const authKeys = createQueryKeys("auth_pages");

export interface AuthProvider {
  key: string;
  label: string;
  start_path: string;
}

export interface OauthResult {
  kind: "error" | "denied";
  message: string;
  retry_path?: string;
}

export interface AuthConfig {
  providers: AuthProvider[];
  password_login: boolean;
  csrf_token: string;
  signed_in: boolean;
  redirect_to: string | null;
  flash: { notice?: string; alert?: string };
  oauth_result: OauthResult | null;
}

export interface SignInResult {
  status: "active" | "pending";
  redirect_to: string;
}

export interface McpAuthorization {
  client: { id: string; name: string };
  redirect_host: string;
  resource: string;
  scopes: string[];
  user: { email: string };
  /** Same-origin form action for the decision (POST /mcp/oauth/consent). */
  decision_path: string;
  csrf_token: string;
}

async function fetchAuthConfig(returnTo?: string | null): Promise<AuthConfig> {
  const body = await apiRequest<DataEnvelope<AuthConfig>>("/api/v1/auth/config", {
    query: { return_to: returnTo ?? undefined },
    noRedirect: true,
  });
  setCsrfToken(body.data.csrf_token);
  return body.data;
}

/**
 * The login bootstrap. Fetched once per page load: the flash and OAuth result
 * it carries are one-shot, so it is never refetched in the background.
 */
export function useAuthConfig(returnTo?: string | null) {
  return useQuery({
    queryKey: authKeys.sub("config", "config", { returnTo: returnTo ?? null }),
    queryFn: () => fetchAuthConfig(returnTo),
    staleTime: Infinity,
    gcTime: Infinity,
    retry: false,
  });
}

/**
 * POST /api/v1/session. A stale CSRF token (signed in or out elsewhere) is
 * refreshed once.
 *
 * The variables hold the password: gcTime 0 drops the mutation from the
 * MutationCache as soon as no component observes it, and callers must
 * reset() it once it settles (copy any error message out first). The password
 * never goes into a query key or the query cache.
 */
export function useSignIn() {
  return useMutation({
    gcTime: 0,
    mutationFn: async (credentials: { email: string; password: string }) => {
      const send = () =>
        apiRequest<DataEnvelope<SignInResult>>("/api/v1/session", {
          method: "POST",
          body: { data: credentials },
          noRedirect: true,
        });
      try {
        return (await send()).data;
      } catch (error) {
        if (!(error instanceof ApiError && error.reason === "csrf")) throw error;
        await fetchAuthConfig();
        return (await send()).data;
      }
    },
  });
}

/** The session for the pending page: null when signed out. */
export function usePendingSession() {
  return useQuery({
    queryKey: authKeys.sub("pending", "session"),
    queryFn: async (): Promise<Me | null> => {
      try {
        const body = await apiRequest<DataEnvelope<Me>>("/api/v1/me", { noRedirect: true });
        setCsrfToken(body.data.csrf_token);
        return body.data;
      } catch (error) {
        if (error instanceof ApiError && error.status === 401) return null;
        throw error;
      }
    },
    retry: false,
  });
}

/** DELETE /api/v1/session. The caller then does a full page load to /login. */
export function useSignOut() {
  return useMutation({
    mutationFn: () => apiRequest<undefined>("/api/v1/session", { method: "DELETE", noRedirect: true }),
  });
}

export function useMcpAuthorization(ticket: string) {
  return useQuery({
    queryKey: authKeys.sub(ticket, "mcp_authorization"),
    queryFn: () =>
      apiRequest<DataEnvelope<McpAuthorization>>("/api/v1/mcp/authorization", { query: { request: ticket } }).then(
        (body) => body.data,
      ),
    enabled: ticket !== "",
    retry: false,
    staleTime: Infinity,
  });
}
