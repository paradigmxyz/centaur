import { describe, expect, it } from 'bun:test';
import { CentaurSessionClient, sessionMetadata, toCodexInputLines, toSessionMessage } from '../src/session-api.js';
import type { FetchFn, GoogleChatApiMessage } from '../src/types.js';

const SPACE_NAME = 'spaces/AAAA1111';
const THREAD_ID = `gchat:${SPACE_NAME}:c3BhY2VzL0FBQUExMTExL3RocmVhZHMvVFRUVA`;

describe('session message mapping', () => {
  it('carries Google Chat identity into session metadata', () => {
    expect(sessionMetadata(messageFixture())).toMatchObject({
      gchat_conversation_name: 'Centaur Demo',
      gchat_thread_name: 'spaces/AAAA1111/threads/TTTT',
      is_mention: true,
      platform: 'googlechat',
      source: 'googlechatbot',
      space_id: SPACE_NAME,
      space_type: 'SPACE',
      user_email: 'casey@example.com',
      user_id: 'users/123',
    });
  });

  it('maps text and attachments into session message parts', () => {
    const message = toSessionMessage(messageFixture({
      attachments: [{ contentType: 'text/csv', name: 'people.csv', resourceName: 'attachments/abc' }],
    }));

    expect(message.role).toBe('user');
    expect(message.parts).toEqual([
      { type: 'text', text: 'Reply exactly PONG.' },
      {
        attachment_type: 'googlechat',
        contentType: 'text/csv',
        mimeType: 'text/csv',
        name: 'people.csv',
        type: 'attachment',
      },
    ]);
  });

  it('describes attachments that could not be downloaded instead of dropping them', () => {
    const [line] = toCodexInputLines(messageFixture({
      attachments: [{
        contentType: 'text/csv',
        fetchError: 'Attachment exceeds 10 bytes',
        name: 'people.csv',
      }],
    }), THREAD_ID);
    const content = JSON.parse(line!).message.content as Array<{ text?: string }>;

    expect(content.at(-1)?.text).toContain('Google Chat attachment was not downloaded: people.csv');
    expect(content.at(-1)?.text).toContain('Attachment exceeds 10 bytes');
  });

  it('stages oversized attachment payloads on their own input lines', () => {
    const lines = toCodexInputLines(messageFixture({
      attachments: [{
        contentType: 'application/pdf',
        dataBase64: 'A'.repeat(1_000_000),
        name: 'big.pdf',
      }],
    }), THREAD_ID);

    expect(lines.length).toBeGreaterThan(1);
    const staged = JSON.parse(lines[0]!);
    expect(staged.type).toBe('attachment.chunk');
    expect(staged.attachmentType).toBe('googlechat');
    for (const line of lines) {
      expect(line.length).toBeLessThanOrEqual(900 * 1024 + 1024);
    }
  });
});

describe('CentaurSessionClient', () => {
  it('retries session creation against the harness the session already runs', async () => {
    const requests: Array<{ body: { harness_type: string }; path: string }> = [];
    const fetchFn: FetchFn = async (input, init) => {
      const path = new URL(String(input)).pathname;
      const body = JSON.parse(String(init?.body));
      requests.push({ body, path });
      if (requests.length === 1) {
        return new Response(JSON.stringify({ existing_harness: 'claude' }), { status: 409 });
      }
      return Response.json({ ok: true });
    };
    const client = new CentaurSessionClient({
      apiUrl: 'http://mock-centaur.local',
      defaultHarnessType: 'codex',
      fetch: fetchFn,
      requestMaxRetries: 0,
    });

    await client.createSession(THREAD_ID, messageFixture());

    expect(requests.map((request) => request.body.harness_type)).toEqual(['codex', 'claude']);
  });

  it('surfaces non-retryable session failures', async () => {
    const client = new CentaurSessionClient({
      apiUrl: 'http://mock-centaur.local',
      fetch: async () => new Response('forbidden', { status: 403 }),
      requestMaxRetries: 0,
    });

    await expect(client.createSession(THREAD_ID, messageFixture())).rejects.toThrow('403');
  });
});

function messageFixture(overrides: Partial<GoogleChatApiMessage> = {}): GoogleChatApiMessage {
  return {
    attachments: [],
    author: {
      email: 'casey@example.com',
      fullName: 'Casey',
      isBot: false,
      userId: 'users/123',
      userName: 'Casey',
    },
    id: 'spaces/AAAA1111/messages/MMMM',
    isMention: true,
    raw: {},
    spaceDisplayName: 'Centaur Demo',
    spaceName: SPACE_NAME,
    spaceType: 'SPACE',
    text: 'Reply exactly PONG.',
    threadId: THREAD_ID,
    threadName: 'spaces/AAAA1111/threads/TTTT',
    timestamp: '2026-06-22T12:00:00.000Z',
    ...overrides,
  };
}
