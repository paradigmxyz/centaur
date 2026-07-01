import { describe, expect, it } from 'bun:test';
import { hydrateTeamsAttachments } from '../src/teams-attachments.js';
import type { TeamsApiAttachment } from '../src/types.js';

describe('Teams attachment hydration', () => {
  it('downloads allowed Teams file download attachments into base64', async () => {
    const hydrated = await hydrateTeamsAttachments([fileDownloadInfoAttachment()], {
      allowedHosts: ['files.example'],
      enabled: true,
      fetchFn: async () => new Response('name,email\nA,a@example.com\n', {
        headers: { 'content-type': 'text/csv' },
        status: 200,
      }),
      maxBytes: 1024,
    });

    expect(hydrated).toHaveLength(1);
    const attachment = hydrated[0];
    if (!attachment) throw new Error('expected hydrated attachment');
    expect(attachment).toMatchObject({
      contentType: 'text/csv',
      contentUrl: undefined,
      dataBase64: Buffer.from('name,email\nA,a@example.com\n').toString('base64'),
      name: 'people.csv',
    });
    expect(JSON.stringify(hydrated)).not.toContain('https://files.example/people.csv');
  });

  it('blocks attachment downloads outside the configured host allow-list', async () => {
    const hydrated = await hydrateTeamsAttachments([{
      contentType: 'text/csv',
      contentUrl: 'https://evil.example/people.csv',
      name: 'people.csv',
    }], {
      allowedHosts: ['files.example'],
      enabled: true,
      fetchFn: async () => new Response('should not fetch'),
      maxBytes: 1024,
    });

    expect(hydrated).toHaveLength(1);
    const attachment = hydrated[0];
    if (!attachment) throw new Error('expected hydrated attachment');
    expect(attachment.dataBase64).toBeUndefined();
    expect(attachment.contentUrl).toBeUndefined();
    expect(attachment.fetchError).toBe('Attachment host is not allowed: evil.example');
  });

  it('redacts Teams download URLs when an allowed download fails', async () => {
    const hydrated = await hydrateTeamsAttachments([fileDownloadInfoAttachment()], {
      allowedHosts: ['files.example'],
      enabled: true,
      fetchFn: async () => new Response('forbidden', { status: 403, statusText: 'Forbidden' }),
      maxBytes: 1024,
    });

    expect(hydrated[0]?.content).toEqual({
      downloadUrlRedacted: true,
      fileName: 'people.csv',
      fileType: 'csv',
    });
    expect(hydrated[0]?.contentUrl).toBeUndefined();
    expect(hydrated[0]?.fetchError).toBe('Attachment download failed: 403 Forbidden');
    expect(JSON.stringify(hydrated)).not.toContain('https://files.example/people.csv');
  });

  it('redacts signed URLs from thrown attachment download errors', async () => {
    const hydrated = await hydrateTeamsAttachments([fileDownloadInfoAttachment()], {
      allowedHosts: ['files.example'],
      enabled: true,
      fetchFn: async () => {
        throw new Error('request failed for https://files.example/people.csv?sig=secret-token');
      },
      maxBytes: 1024,
    });

    expect(hydrated[0]?.contentUrl).toBeUndefined();
    expect(hydrated[0]?.fetchError).toBe('request failed for [redacted-url:files.example]');
    expect(JSON.stringify(hydrated)).not.toContain('https://files.example/people.csv');
    expect(JSON.stringify(hydrated)).not.toContain('secret-token');
  });

  it('redacts Teams download URLs when downloads are disabled or blocked', async () => {
    const disabled = await hydrateTeamsAttachments([fileDownloadInfoAttachment()], {
      allowedHosts: ['files.example'],
      enabled: false,
      maxBytes: 1024,
    });
    const blocked = await hydrateTeamsAttachments([fileDownloadInfoAttachment()], {
      allowedHosts: ['other.example'],
      enabled: true,
      fetchFn: async () => new Response('should not fetch'),
      maxBytes: 1024,
    });

    expect(disabled[0]?.content).toEqual({
      downloadUrlRedacted: true,
      fileName: 'people.csv',
      fileType: 'csv',
    });
    expect(blocked[0]?.content).toEqual({
      downloadUrlRedacted: true,
      fileName: 'people.csv',
      fileType: 'csv',
    });
    expect(JSON.stringify(disabled)).not.toContain('https://files.example/people.csv');
    expect(JSON.stringify(blocked)).not.toContain('https://files.example/people.csv');
  });

  it('stops reading streamed attachments after the byte limit', async () => {
    let canceled = false;
    const hydrated = await hydrateTeamsAttachments([fileDownloadInfoAttachment()], {
      allowedHosts: ['files.example'],
      enabled: true,
      fetchFn: async () => new Response(new ReadableStream<Uint8Array>({
        cancel: () => {
          canceled = true;
        },
        start(controller) {
          controller.enqueue(new Uint8Array([1, 2, 3]));
          controller.enqueue(new Uint8Array([4, 5, 6]));
        },
      }), { status: 200 }),
      maxBytes: 4,
    });

    expect(hydrated[0]?.dataBase64).toBeUndefined();
    expect(hydrated[0]?.contentUrl).toBeUndefined();
    expect(hydrated[0]?.fetchError).toBe('Attachment exceeds 4 bytes');
    expect(canceled).toBe(true);
  });

  it('uses a Graph token fallback for Graph-backed attachment URLs', async () => {
    const seenAuth: string[] = [];
    const seenScopes: string[] = [];
    const hydrated = await hydrateTeamsAttachments([{
      contentType: 'text/csv',
      contentUrl: 'https://graph.microsoft.com/v1.0/shares/share-id/driveItem/content',
      name: 'people.csv',
    }], {
      allowedHosts: ['graph.microsoft.com'],
      enabled: true,
      fetchFn: async (_url, init) => {
        const auth = new Headers(init?.headers).get('authorization');
        seenAuth.push(auth ?? '');
        if (!auth) {
          return new Response('forbidden', { status: 403 });
        }
        return new Response('ok', { headers: { 'content-type': 'text/csv' }, status: 200 });
      },
      graphTokenProvider: {
        getAccessToken: async (scope) => {
          seenScopes.push(scope ?? '');
          return 'graph-token';
        },
      },
      graphTokenScope: 'https://graph.microsoft.us/.default',
      maxBytes: 1024,
    });

    expect(seenAuth).toEqual(['', 'Bearer graph-token']);
    expect(seenScopes).toEqual(['https://graph.microsoft.us/.default']);
    expect(hydrated).toHaveLength(1);
    const attachment = hydrated[0];
    if (!attachment) throw new Error('expected hydrated attachment');
    expect(attachment.dataBase64).toBe(Buffer.from('ok').toString('base64'));
  });
});

function fileDownloadInfoAttachment(): TeamsApiAttachment {
  return {
    content: {
      downloadUrl: 'https://files.example/people.csv',
      fileName: 'people.csv',
      fileType: 'csv',
    },
    contentType: 'application/vnd.microsoft.teams.file.download.info',
    name: 'people.csv',
  };
}
