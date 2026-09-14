import { describe, expect, it } from 'bun:test';
import type { Attachment } from 'chat';
import { hydrateGoogleChatAttachments } from '../src/googlechat-attachments.js';
import type { GoogleChatApiAttachment } from '../src/types.js';

const attachment: GoogleChatApiAttachment = {
  contentType: 'text/csv',
  name: 'people.csv',
  resourceName: 'attachments/abc',
};

describe('hydrateGoogleChatAttachments', () => {
  it('leaves attachments untouched when downloads are disabled', async () => {
    const hydrated = await hydrateGoogleChatAttachments([attachment], [sdkAttachment()], {
      enabled: false,
      maxBytes: 1024,
    });

    expect(hydrated).toEqual([attachment]);
  });

  it('downloads through the adapter data reference', async () => {
    const hydrated = await hydrateGoogleChatAttachments([attachment], [sdkAttachment()], {
      enabled: true,
      maxBytes: 1024,
    });

    expect(hydrated[0]?.dataBase64).toBe(Buffer.from('name,role\n').toString('base64'));
    expect(hydrated[0]?.fetchError).toBeUndefined();
  });

  it('drops payloads over the size cap instead of forwarding them', async () => {
    const hydrated = await hydrateGoogleChatAttachments([attachment], [sdkAttachment()], {
      enabled: true,
      maxBytes: 3,
    });

    expect(hydrated[0]?.dataBase64).toBeUndefined();
    expect(hydrated[0]?.fetchError).toBe('Attachment exceeds 3 bytes');
  });

  it('reports attachments the app cannot read', async () => {
    const hydrated = await hydrateGoogleChatAttachments([attachment], [{ type: 'file' }], {
      enabled: true,
      maxBytes: 1024,
    });

    expect(hydrated[0]?.fetchError).toBe('Attachment has no Google Chat data reference to download');
  });

  it('redacts URLs from download failures', async () => {
    const hydrated = await hydrateGoogleChatAttachments([attachment], [{
      fetchData: async () => {
        throw new Error('GET https://chat.googleapis.com/v1/media/abc?token=secret failed');
      },
      type: 'file',
    }], { enabled: true, maxBytes: 1024 });

    expect(hydrated[0]?.fetchError).toContain('[redacted-url:chat.googleapis.com]');
    expect(hydrated[0]?.fetchError).not.toContain('token=secret');
  });
});

function sdkAttachment(): Attachment {
  return {
    fetchData: async () => Buffer.from('name,role\n'),
    mimeType: 'text/csv',
    name: 'people.csv',
    type: 'file',
  };
}
