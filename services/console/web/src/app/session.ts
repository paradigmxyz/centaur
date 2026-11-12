// The signed-in caller, from GET /api/v1/me. The shell loads it once; every
// component reads it through useSession() (never fetch /me directly).
import { useSuspenseQuery, type QueryClient } from "@tanstack/react-query";

import { api, setCsrfToken, type DataEnvelope } from "@/lib/api";
import { sharedKeys } from "@/lib/query-keys";

export type NavSection =
  | "control"
  | "principals"
  | "roles"
  | "secrets"
  | "credentials"
  | "oauth_apps"
  | "users"
  | "api_clients"
  | "settings"
  | "workflows"
  | "integrations"
  | "skills"
  | "scheduled_tasks";

export interface Me {
  user: { id: string; email: string; name: string | null; status: "active" | "pending" | "disabled"; admin: boolean };
  auth_method: "session" | "api_key";
  acting_admin: boolean;
  descoped: boolean;
  csrf_token: string | null;
  features: { api_clients: boolean; password_login: boolean };
  nav: Record<NavSection, boolean>;
  flash: { notice?: string; alert?: string };
}

export async function fetchMe(): Promise<Me> {
  const body = await api.get<DataEnvelope<Me>>("/api/v1/me");
  setCsrfToken(body.data.csrf_token);
  return body.data;
}

export const meQueryOptions = {
  queryKey: sharedKeys.me,
  queryFn: fetchMe,
  staleTime: 60_000,
} as const;

/** The session inside the authenticated shell (suspends until loaded). */
export function useSession(): Me {
  return useSuspenseQuery(meQueryOptions).data;
}

/** Drops every cached query except the session (data loaded under other permissions). */
function removeNonSessionQueries(queryClient: QueryClient) {
  queryClient.removeQueries({ predicate: (query) => query.queryKey[0] !== sharedKeys.me[0] });
}

/** POST /api/v1/descope ("view as operator"); returns the refreshed session. */
export async function descope(queryClient: QueryClient): Promise<Me> {
  await api.post("/api/v1/descope");
  removeNonSessionQueries(queryClient);
  return queryClient.fetchQuery({ queryKey: sharedKeys.me, queryFn: fetchMe });
}

/** DELETE /api/v1/descope: restore admin permissions. */
export async function restoreAdmin(queryClient: QueryClient) {
  await api.delete("/api/v1/descope");
  removeNonSessionQueries(queryClient);
  await queryClient.invalidateQueries({ queryKey: sharedKeys.me });
}

/** DELETE /api/v1/session; the caller then leaves the SPA. */
export async function signOut(queryClient: QueryClient) {
  await api.delete("/api/v1/session");
  queryClient.clear();
}

/** Where a signed-in user lands: admins on Principals, everyone else on Integrations. */
export function defaultLandingPath(me: Pick<Me, "acting_admin">): string {
  return me.acting_admin ? "/console/principals" : "/console/integrations";
}
