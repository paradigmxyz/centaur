import type { TeamsbotConfig } from './config.js';
import type { FetchFn } from './types.js';

export type GraphTokenProvider = {
  getAccessToken(scope?: string): Promise<string | undefined>;
};

export function createGraphTokenProvider(config: TeamsbotConfig, fetchFn: FetchFn = fetch): GraphTokenProvider {
  const cache = new Map<string, { expiresAt: number; token: string }>();
  return {
    async getAccessToken(scope = config.teams.graphTokenScope): Promise<string | undefined> {
      if (config.teams.graphBearerToken) {
        return config.teams.graphBearerToken;
      }
      if (!config.teams.appId || !config.teams.appPassword || !config.teams.appTenantId) {
        return undefined;
      }
      const now = Date.now();
      const cached = cache.get(scope);
      if (cached && cached.expiresAt - 60_000 > now) {
        return cached.token;
      }
      const body = new URLSearchParams({
        client_id: config.teams.appId,
        client_secret: config.teams.appPassword,
        grant_type: 'client_credentials',
        scope,
      });
      const response = await fetchFn(`https://login.microsoftonline.com/${encodeURIComponent(config.teams.appTenantId)}/oauth2/v2.0/token`, {
        method: 'POST',
        headers: { 'content-type': 'application/x-www-form-urlencoded' },
        body,
      });
      if (!response.ok) {
        await response.body?.cancel();
        return undefined;
      }
      const payload = await response.json() as { access_token?: unknown; expires_in?: unknown };
      if (typeof payload.access_token !== 'string' || !payload.access_token) {
        return undefined;
      }
      const expiresInSeconds = typeof payload.expires_in === 'number' ? payload.expires_in : 3600;
      cache.set(scope, {
        expiresAt: now + expiresInSeconds * 1000,
        token: payload.access_token,
      });
      return payload.access_token;
    },
  };
}
