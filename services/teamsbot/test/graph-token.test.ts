import { describe, expect, it } from 'bun:test';
import type { TeamsbotConfig } from '../src/config.js';
import { createGraphTokenProvider } from '../src/graph-token.js';

const config: TeamsbotConfig = {
  centaur: { apiUrl: 'http://mock-centaur.local', requestMaxRetries: 0, requestRetryDelayMs: 0 },
  server: { logLevel: 'silent', port: 0 },
  teams: {
    allowedChannelIds: [],
    allowedTeamIds: [],
    allowedTenantIds: [],
    appId: 'bot-id',
    appPassword: 'bot-password',
    appTenantId: 'tenant-1',
    attachmentAllowedHosts: ['graph.microsoft.com'],
    attachmentDownloadEnabled: false,
    attachmentMaxBytes: 1024 * 1024,
    activeExecutionTtlMs: 30 * 60 * 1000,
    defaultHarnessType: 'codex',
    graphTokenScope: 'https://graph.microsoft.com/.default',
    renderDeliveryTimeoutMs: 15_000,
    requireMention: true,
  },
};

describe('Graph token provider', () => {
  it('caches tokens separately by scope', async () => {
    const requestedScopes: string[] = [];
    const provider = createGraphTokenProvider(config, async (_input, init) => {
      const body = new URLSearchParams(String(init?.body));
      const scope = body.get('scope') ?? '';
      requestedScopes.push(scope);
      return Response.json({
        access_token: `token:${scope}`,
        expires_in: 3600,
      });
    });

    await expect(provider.getAccessToken('scope-a')).resolves.toBe('token:scope-a');
    await expect(provider.getAccessToken('scope-b')).resolves.toBe('token:scope-b');
    await expect(provider.getAccessToken('scope-a')).resolves.toBe('token:scope-a');

    expect(requestedScopes).toEqual(['scope-a', 'scope-b']);
  });
});
