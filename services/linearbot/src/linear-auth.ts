import type { LinearbotOptions } from "./types";

/**
 * Scopes for the client-credentials token. The adapter's default omits
 * `app:assignable`, which issue delegation to the bot needs.
 */
export const DEFAULT_CLIENT_CREDENTIAL_SCOPES = [
  "read",
  "write",
  "comments:create",
  "issues:create",
  "app:mentionable",
  "app:assignable",
];

export type LinearAdapterAuth =
  | {
      clientCredentials: {
        clientId: string;
        clientSecret: string;
        scopes: string[];
      };
    }
  | { accessToken: string }
  | { apiKey: string }
  | Record<string, never>;

/**
 * Adapter auth config: client credentials (the adapter mints and refreshes its
 * own token) win over a static access token, which wins over an API key.
 */
export function linearAdapterAuth(
  options: Pick<
    LinearbotOptions,
    "linearAccessToken" | "linearApiKey" | "linearClientCredentials"
  >,
): LinearAdapterAuth {
  const credentials = options.linearClientCredentials;
  if (credentials) {
    return {
      clientCredentials: {
        clientId: credentials.clientId,
        clientSecret: credentials.clientSecret,
        scopes: credentials.scopes?.length
          ? credentials.scopes
          : DEFAULT_CLIENT_CREDENTIAL_SCOPES,
      },
    };
  }
  if (options.linearAccessToken) {
    return { accessToken: options.linearAccessToken };
  }
  if (options.linearApiKey) return { apiKey: options.linearApiKey };
  return {};
}
