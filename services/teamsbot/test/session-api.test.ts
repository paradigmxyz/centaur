import { describe, expect, it } from 'bun:test';
import { CentaurSessionClient, toCodexInputLines, toSessionMessage } from '../src/session-api.js';
import type { TeamsApiMessage } from '../src/types.js';

const THREAD_ID = 'teams:Y29udmVyc2F0aW9uLTE:aHR0cHM6Ly9zbWJhLnRyYWZmaWNtYW5hZ2VyLm5ldC9hbWVyLw';

describe('session API serialization', () => {
  it('builds Codex input lines with Teams trace metadata', () => {
    const lines = toCodexInputLines(messageFixture(), THREAD_ID);
    const payload = JSON.parse(lines.at(-1)!);

    expect(payload.thread_key).toBe(THREAD_ID);
    expect(payload.trace_metadata.platform).toBe('msteams');
    expect(payload.message.content).toContainEqual({ type: 'text', text: 'hello' });
  });

  it('stores attachments as session message parts', () => {
    const message = toSessionMessage({
      ...messageFixture(),
      attachments: [{ contentType: 'text/csv', contentUrl: 'https://files.example/people.csv', name: 'people.csv' }],
    });

    expect(message.parts).toContainEqual({
      attachment_type: 'teams',
      contentType: 'text/csv',
      mimeType: 'text/csv',
      name: 'people.csv',
      type: 'attachment',
      url: undefined,
    });
  });

  it('carries Teams personal chat display metadata for principal registration', () => {
    const message = toSessionMessage({
      ...messageFixture(),
      author: { aadObjectId: 'aad-user-1', fullName: 'Casey Harper', isBot: false, userId: 'user-1', userName: 'Casey' },
      conversationType: 'personal',
      teamId: undefined,
      threadId: THREAD_ID,
    });

    expect(message.metadata).toMatchObject({
      aad_object_id: 'aad-user-1',
      teams_conversation_name: 'Casey Harper',
      thread_id: THREAD_ID,
    });
  });

  it('sends Teams identity metadata during session creation for principal registration', async () => {
    let createBody: any;
    const client = new CentaurSessionClient({
      apiUrl: 'http://mock-centaur.local',
      fetch: async (_input, init) => {
        createBody = JSON.parse(String(init?.body));
        return Response.json({ ok: true });
      },
    });

    await client.createSession(THREAD_ID, {
      ...messageFixture(),
      author: { isBot: false, userId: 'teams-user-1', userName: 'Casey' },
      conversationType: 'personal',
      teamId: undefined,
      threadId: THREAD_ID,
    });

    expect(createBody.metadata).toMatchObject({
      channel_id: 'channel-1',
      conversation_id: 'conversation-1',
      teams_conversation_name: 'Casey',
      teams_user_id: 'teams-user-1',
      user_id: 'teams-user-1',
    });
    expect(createBody.metadata).not.toHaveProperty('tenant_id');
  });

  it('retries session creation with the existing harness after an implicit default conflict', async () => {
    const createBodies: any[] = [];
    const client = new CentaurSessionClient({
      apiUrl: 'http://mock-centaur.local',
      defaultHarnessType: 'claudecode',
      fetch: async (_input, init) => {
        createBodies.push(JSON.parse(String(init?.body)));
        if (createBodies.length === 1) {
          return Response.json({
            code: 'harness_conflict',
            existing_harness: 'codex',
            ok: false,
            requested_harness: 'claudecode',
          }, { status: 409 });
        }
        return Response.json({ ok: true });
      },
    });

    await client.createSession('thread-1', messageFixture());

    expect(createBodies.map((body) => body.harness_type)).toEqual(['claudecode', 'codex']);
  });

  it('recovers the existing harness from a conflict error message', async () => {
    const createBodies: any[] = [];
    const client = new CentaurSessionClient({
      apiUrl: 'http://mock-centaur.local',
      defaultHarnessType: 'codex',
      fetch: async (_input, init) => {
        createBodies.push(JSON.parse(String(init?.body)));
        if (createBodies.length === 1) {
          return Response.json({
            error: 'session thread-1 already exists with harness_type amp, requested codex',
            ok: false,
          }, { status: 409 });
        }
        return Response.json({ ok: true });
      },
    });

    await client.createSession('thread-1', messageFixture());

    expect(createBodies.map((body) => body.harness_type)).toEqual(['codex', 'amp']);
  });

  it('sends downloaded Teams attachments with the harness mimeType field', () => {
    const lines = toCodexInputLines({
      ...messageFixture(),
      attachments: [{
        contentType: 'text/csv',
        contentUrl: 'https://files.example/people.csv',
        dataBase64: Buffer.from('name,email\nA,a@example.com\n').toString('base64'),
        name: 'people.csv',
      }],
    }, THREAD_ID);
    const payload = JSON.parse(lines.at(-1)!);

    expect(payload.message.content).toContainEqual(expect.objectContaining({
      attachment_type: 'teams',
      dataBase64: Buffer.from('name,email\nA,a@example.com\n').toString('base64'),
      mimeType: 'text/csv',
      name: 'people.csv',
      type: 'attachment',
    }));
    expect(JSON.stringify(payload)).not.toContain('https://files.example/people.csv');
  });

  it('describes undownloaded Teams attachments as Teams text instead of generic attachment blocks', () => {
    const lines = toCodexInputLines({
      ...messageFixture(),
      attachments: [{
        contentType: 'text/csv',
        contentUrl: 'https://files.example/people.csv',
        fetchError: 'Attachment download failed: 403 Forbidden',
        name: 'people.csv',
      }],
    }, THREAD_ID);
    const payload = JSON.parse(lines.at(-1)!);

    expect(payload.message.content).toContainEqual({
      type: 'text',
      text: [
        'Teams attachment was not downloaded: people.csv',
        'Content-Type: text/csv',
        'Download error: Attachment download failed: 403 Forbidden',
      ].join('\n'),
    });
    expect(JSON.stringify(payload)).not.toContain('https://files.example/people.csv');
    expect(payload.message.content).not.toContainEqual(expect.objectContaining({ type: 'attachment' }));
  });

  it('cancels the event stream reader when the consumer stops early', async () => {
    let canceled = false;
    const encoder = new TextEncoder();
    const client = new CentaurSessionClient({
      apiUrl: 'http://mock-centaur.local',
      fetch: async () => new Response(new ReadableStream<Uint8Array>({
        cancel: () => {
          canceled = true;
        },
        start(controller) {
          controller.enqueue(encoder.encode('id: 1\nevent: session.output.line\ndata: {}\n\n'));
        },
      }), {
        headers: { 'content-type': 'text/event-stream' },
        status: 200,
      }),
    });

    const events = await client.streamEvents({
      afterEventId: 0,
      onEventId: () => undefined,
      threadId: 'thread-1',
    });
    const iterator = events[Symbol.asyncIterator]();

    await expect(iterator.next()).resolves.toMatchObject({ done: false });
    await iterator.return?.();

    expect(canceled).toBe(true);
  });

  it('normalizes terminal SSE failure and cancellation payloads', async () => {
    const client = new CentaurSessionClient({
      apiUrl: 'http://mock-centaur.local',
      fetch: async () => sseResponse([
        'id: 1',
        'event: session.execution_failed',
        'data: {"message":"tool exploded"}',
        '',
      ]),
    });

    const events = await collectStream(await client.streamEvents({
      afterEventId: 0,
      onEventId: () => undefined,
      threadId: 'thread-1',
    }));

    expect(events).toEqual([{
      data: { error: 'tool exploded' },
      event: 'session.execution_failed',
      eventId: 1,
      eventKind: 'session.execution_failed',
    }]);

    const cancelled = new CentaurSessionClient({
      apiUrl: 'http://mock-centaur.local',
      fetch: async () => sseResponse([
        'id: 2',
        'event: session.execution_cancelled',
        'data: {}',
        '',
      ]),
    });

    await expect(collectStream(await cancelled.streamEvents({
      afterEventId: 0,
      onEventId: () => undefined,
      threadId: 'thread-1',
    }))).resolves.toEqual([{
      data: { error: 'Execution cancelled' },
      event: 'session.execution_cancelled',
      eventId: 2,
      eventKind: 'session.execution_cancelled',
    }]);
  });

  it('parses terminal SSE completion payloads', async () => {
    const client = new CentaurSessionClient({
      apiUrl: 'http://mock-centaur.local',
      fetch: async () => sseResponse([
        'id: 3',
        'event: session.execution_completed',
        'data: {"result_text":"done"}',
        '',
      ]),
    });

    await expect(collectStream(await client.streamEvents({
      afterEventId: 0,
      onEventId: () => undefined,
      threadId: 'thread-1',
    }))).resolves.toEqual([{
      data: { result_text: 'done' },
      event: 'session.execution_completed',
      eventId: 3,
      eventKind: 'session.execution_completed',
    }]);
  });

  it('flushes terminal SSE frames that end without a blank line', async () => {
    const client = new CentaurSessionClient({
      apiUrl: 'http://mock-centaur.local',
      fetch: async () => sseResponse([
        'id: 4',
        'event: session.stream_error',
        'data: {"error":"lost stream"}',
      ]),
    });

    await expect(collectStream(await client.streamEvents({
      afterEventId: 0,
      onEventId: () => undefined,
      threadId: 'thread-1',
    }))).resolves.toEqual([{
      data: { error: 'lost stream' },
      event: 'session.stream_error',
      eventId: 4,
      eventKind: 'session.stream_error',
    }]);
  });

  it('retries retryable Centaur API failures', async () => {
    const calls: string[] = [];
    const client = new CentaurSessionClient({
      apiUrl: 'http://mock-centaur.local',
      fetch: async (_input, init) => {
        calls.push(init?.method ?? 'GET');
        if (calls.length === 1) {
          return new Response('try again', { status: 503, statusText: 'Service Unavailable' });
        }
        return Response.json({ ok: true });
      },
      requestMaxRetries: 1,
      requestRetryDelayMs: 0,
    });

    await client.createSession('thread-1', messageFixture());

    expect(calls).toEqual(['POST', 'POST']);
  });
});

async function collectStream<T>(stream: AsyncIterable<T>): Promise<T[]> {
  const items: T[] = [];
  for await (const item of stream) {
    items.push(item);
  }
  return items;
}

function sseResponse(lines: string[]): Response {
  const encoder = new TextEncoder();
  return new Response(new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(encoder.encode(`${lines.join('\n')}\n`));
      controller.close();
    },
  }), {
    headers: { 'content-type': 'text/event-stream' },
    status: 200,
  });
}

function messageFixture(): TeamsApiMessage {
  return {
    attachments: [],
    author: { isBot: false, userId: 'user-1', userName: 'Casey' },
    channelId: 'channel-1',
    conversationId: 'conversation-1',
    id: 'message-1',
    isMention: true,
    raw: {},
    teamId: 'team-1',
    tenantId: 'tenant-1',
    text: 'hello',
    threadId: THREAD_ID,
    timestamp: '2026-06-16T00:00:00.000Z',
  };
}
