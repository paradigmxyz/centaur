import { describe, expect, it } from 'bun:test';
import type { Thread } from 'chat';
import type { GoogleChatbotConfig } from '../src/config.js';
import {
  decodeGoogleChatThreadKey,
  encodeGoogleChatThreadKey,
  isAllowedGoogleChatMessage,
  serializeGoogleChatMessage,
} from '../src/googlechat-message.js';
import { GoogleChatbotService, hasLiveActiveExecution } from '../src/googlechatbot.js';
import { createRenderRecoveryScheduler } from '../src/index.js';
import { CentaurSessionClient } from '../src/session-api.js';
import type {
  GoogleChatApiMessage,
  GoogleChatEventEnvelope,
  GoogleChatThreadState,
} from '../src/types.js';
import { InMemoryGoogleChatThreadStateStore } from './support/in-memory-state.js';
import { createMockCentaurFetch } from './support/mock-centaur.js';

const SPACE_NAME = 'spaces/AAAA1111';
const THREAD_NAME = 'spaces/AAAA1111/threads/TTTT';
const THREAD_ID = `gchat:${SPACE_NAME}:${Buffer.from(THREAD_NAME).toString('base64url')}`;
const MESSAGE_ID = 'spaces/AAAA1111/messages/MMMM';

const config: GoogleChatbotConfig = {
  centaur: { apiUrl: 'http://mock-centaur.local', requestMaxRetries: 0, requestRetryDelayMs: 0 },
  gchat: {
    activeExecutionTtlMs: 30 * 60 * 1000,
    allowDirectMessages: false,
    allowedDomains: [],
    allowedSenderEmails: [],
    allowedSpaceIds: [SPACE_NAME],
    attachmentDownloadEnabled: false,
    attachmentMaxBytes: 1024 * 1024,
    defaultHarnessType: 'codex',
    projectNumber: '1234567890',
    renderDeliveryTimeoutMs: 15_000,
    renderMinEditIntervalMs: 0,
    requireMention: true,
    useApplicationDefaultCredentials: true,
    userName: 'centaur',
  },
  server: { logLevel: 'silent', port: 0 },
};

describe('google chat thread keys', () => {
  it('round-trips the adapter space/thread/dm encoding', () => {
    expect(decodeGoogleChatThreadKey(THREAD_ID)).toEqual({
      isDm: false,
      spaceName: SPACE_NAME,
      threadName: THREAD_NAME,
    });
    expect(decodeGoogleChatThreadKey(`gchat:${SPACE_NAME}:dm`)).toEqual({
      isDm: true,
      spaceName: SPACE_NAME,
    });
    expect(encodeGoogleChatThreadKey({ isDm: false, spaceName: SPACE_NAME, threadName: THREAD_NAME })).toBe(THREAD_ID);
    expect(encodeGoogleChatThreadKey({ isDm: true, spaceName: SPACE_NAME })).toBe(`gchat:${SPACE_NAME}:dm`);
  });

  it('rejects keys from other platforms', () => {
    expect(decodeGoogleChatThreadKey('slack:C123:1.2')).toBeUndefined();
    expect(decodeGoogleChatThreadKey('gchat:')).toBeUndefined();
  });
});

describe('google chat policy', () => {
  it('fails closed when no allowlist is configured', () => {
    expect(isAllowedGoogleChatMessage({
      allowDirectMessages: false,
      allowedDomains: [],
      allowedSenderEmails: [],
      allowedSpaceIds: [],
      message: messageFixture(),
      threadKey: THREAD_ID,
    })).toBe(false);
  });

  it('allows an allowlisted space', () => {
    expect(isAllowedGoogleChatMessage({
      allowDirectMessages: false,
      allowedDomains: [],
      allowedSenderEmails: [],
      allowedSpaceIds: [SPACE_NAME],
      message: messageFixture(),
      threadKey: THREAD_ID,
    })).toBe(true);
  });

  it('denies a space outside the allowlist', () => {
    expect(isAllowedGoogleChatMessage({
      allowDirectMessages: false,
      allowedDomains: [],
      allowedSenderEmails: [],
      allowedSpaceIds: ['spaces/OTHER'],
      message: messageFixture(),
      threadKey: THREAD_ID,
    })).toBe(false);
  });

  it('applies the sender domain allowlist on top of the space allowlist', () => {
    const allow = {
      allowDirectMessages: false,
      allowedDomains: ['example.com'],
      allowedSenderEmails: [],
      allowedSpaceIds: [SPACE_NAME],
      threadKey: THREAD_ID,
    };
    expect(isAllowedGoogleChatMessage({ ...allow, message: messageFixture() })).toBe(true);
    expect(isAllowedGoogleChatMessage({
      ...allow,
      message: messageFixture({ author: { ...messageFixture().author, email: 'intruder@evil.test' } }),
    })).toBe(false);
    expect(isAllowedGoogleChatMessage({
      ...allow,
      message: messageFixture({ author: { ...messageFixture().author, email: undefined } }),
    })).toBe(false);
  });

  it('keeps direct messages off until both the DM flag and a sender allowlist are set', () => {
    const dmMessage = messageFixture({ spaceType: 'DIRECT_MESSAGE' });
    const dmThreadKey = `gchat:${SPACE_NAME}:dm`;
    expect(isAllowedGoogleChatMessage({
      allowDirectMessages: false,
      allowedDomains: ['example.com'],
      allowedSenderEmails: [],
      allowedSpaceIds: [SPACE_NAME],
      message: dmMessage,
      threadKey: dmThreadKey,
    })).toBe(false);
    expect(isAllowedGoogleChatMessage({
      allowDirectMessages: true,
      allowedDomains: [],
      allowedSenderEmails: [],
      allowedSpaceIds: [SPACE_NAME],
      message: dmMessage,
      threadKey: dmThreadKey,
    })).toBe(false);
    expect(isAllowedGoogleChatMessage({
      allowDirectMessages: true,
      allowedDomains: ['example.com'],
      allowedSenderEmails: [],
      allowedSpaceIds: [],
      message: dmMessage,
      threadKey: dmThreadKey,
    })).toBe(true);
  });
});

describe('google chat message serialization', () => {
  it('reads the Workspace Add-on webhook shape', () => {
    const message = serializeGoogleChatMessage(eventFixture(), THREAD_ID, 'Reply exactly PONG.');
    expect(message).toMatchObject({
      id: MESSAGE_ID,
      spaceDisplayName: 'Centaur Demo',
      spaceName: SPACE_NAME,
      spaceType: 'SPACE',
      text: 'Reply exactly PONG.',
      threadName: THREAD_NAME,
    });
    expect(message.author).toMatchObject({
      email: 'casey@example.com',
      isBot: false,
      userId: 'users/123',
      userName: 'Casey',
    });
  });

  it('reads the Workspace Events (Pub/Sub) notification shape', () => {
    const message = serializeGoogleChatMessage({
      eventType: 'google.workspace.chat.message.v1.created',
      message: eventFixture().chat!.messagePayload!.message,
      space: { name: SPACE_NAME, spaceType: 'SPACE' },
    }, THREAD_ID, 'hello');
    expect(message.spaceName).toBe(SPACE_NAME);
    expect(message.id).toBe(MESSAGE_ID);
  });

  it('redacts attachment download URIs from the stored raw event', () => {
    const message = serializeGoogleChatMessage(eventFixture({
      attachment: [{
        attachmentDataRef: { resourceName: 'attachments/abc' },
        contentName: 'people.csv',
        contentType: 'text/csv',
        downloadUri: 'https://chat.google.com/download?token=secret',
      }],
    }), THREAD_ID, 'check this');

    expect(JSON.stringify(message.raw)).not.toContain('token=secret');
    expect(JSON.stringify(message.raw)).toContain('downloadUriRedacted');
    expect(message.attachments[0]).toMatchObject({
      contentType: 'text/csv',
      name: 'people.csv',
      resourceName: 'attachments/abc',
    });
    expect(message.attachments[0]?.downloadUri).toBeUndefined();
  });
});

describe('GoogleChatbotService', () => {
  it('treats only timestamped in-flight executions inside the TTL as live', () => {
    expect(hasLiveActiveExecution({ active: true, activeExecution: true, activeExecutionStartedAt: 1_000 }, 1_000, 1_500)).toBe(true);
    expect(hasLiveActiveExecution({ active: true, activeExecution: true, activeExecutionStartedAt: 1_000 }, 1_000, 2_001)).toBe(false);
    expect(hasLiveActiveExecution({ active: true, activeExecution: true }, 1_000, 1_500)).toBe(false);
  });

  it('reschedules render recovery after an idle scan exits', async () => {
    let scanCount = 0;
    const scheduler = createRenderRecoveryScheduler({
      logger: { error: () => undefined, warn: () => undefined } as never,
      recoverRenderObligations: async () => {
        scanCount += 1;
        return 0;
      },
    });

    scheduler.schedule();
    await waitFor(() => scanCount === 1);
    scheduler.schedule();
    await waitFor(() => scanCount === 2);

    expect(scanCount).toBe(2);
  });

  it('executes a mentioned message and edits the progress message with the answer', async () => {
    const mock = createMockCentaurFetch('PONG');
    const thread = createThread();
    const service = new GoogleChatbotService(
      config,
      new InMemoryGoogleChatThreadStateStore(),
      new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
    );

    await service.runChatMessage(thread, chatMessageFixture(), 'execute');

    expect(mock.requests.map((request) => `${request.method} ${request.path}`)).toEqual([
      `POST /api/session/${encodeURIComponent(THREAD_ID)}`,
      `POST /api/session/${encodeURIComponent(THREAD_ID)}/messages`,
      `POST /api/session/${encodeURIComponent(THREAD_ID)}/execute`,
      `GET /api/session/${encodeURIComponent(THREAD_ID)}/events`,
    ]);
    expect(thread.posts).toEqual(['Thinking...']);
    expect(thread.edits.at(-1)).toEqual({ id: `${SPACE_NAME}/messages/POST-1`, text: 'PONG' });
  });

  it('ignores messages from spaces outside the allowlist', async () => {
    const mock = createMockCentaurFetch('PONG');
    const thread = createThread();
    const service = new GoogleChatbotService(
      { ...config, gchat: { ...config.gchat, allowedSpaceIds: ['spaces/OTHER'] } },
      new InMemoryGoogleChatThreadStateStore(),
      new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
    );

    await service.runChatMessage(thread, chatMessageFixture(), 'execute');

    expect(mock.requests).toEqual([]);
    expect(thread.posts).toEqual([]);
  });

  it('ignores messages sent by the app itself', async () => {
    const mock = createMockCentaurFetch('PONG');
    const thread = createThread();
    const service = new GoogleChatbotService(
      config,
      new InMemoryGoogleChatThreadStateStore(),
      new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
    );

    await service.runChatMessage(thread, chatMessageFixture({
      event: eventFixture({ sender: { name: 'users/bot', displayName: 'Centaur', type: 'BOT' } }),
    }), 'execute');

    expect(mock.requests).toEqual([]);
  });

  it('stores the space reference so a restart can finish the render', async () => {
    const mock = createMockCentaurFetch('PONG');
    const stateStore = new InMemoryGoogleChatThreadStateStore();
    const thread = createThread();
    const service = new GoogleChatbotService(
      config,
      stateStore,
      new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
    );

    await service.runChatMessage(thread, chatMessageFixture(), 'execute');

    await expect(stateStore.getReference(THREAD_ID)).resolves.toMatchObject({
      isDm: false,
      spaceDisplayName: 'Centaur Demo',
      spaceName: SPACE_NAME,
      threadName: THREAD_NAME,
    });
  });

  it('appends instead of executing while a render obligation is active', async () => {
    const mock = createMockCentaurFetch('PONG');
    const thread = createThread({
      state: {
        active: true,
        activeExecution: true,
        activeExecutionStartedAt: Date.now(),
        renderObligation: {
          afterEventId: 0,
          executionId: 'exec-1',
          message: messageFixture(),
          progressMessageId: `${SPACE_NAME}/messages/POST-1`,
        },
      },
    });
    const service = new GoogleChatbotService(
      config,
      new InMemoryGoogleChatThreadStateStore(),
      new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
    );

    await service.runChatMessage(thread, chatMessageFixture({
      id: `${SPACE_NAME}/messages/MMMM2`,
      isMention: false,
      text: 'additional context',
    }), 'append');

    expect(mock.requests.map((request) => `${request.method} ${request.path}`)).toEqual([
      `POST /api/session/${encodeURIComponent(THREAD_ID)}`,
      `POST /api/session/${encodeURIComponent(THREAD_ID)}/messages`,
    ]);
    expect(thread.stateValue.forwardedMessageIds).toEqual([`${SPACE_NAME}/messages/MMMM2`]);
    expect(thread.posts).toEqual([]);
  });

  it('ignores duplicate webhook redelivery after the message executed', async () => {
    const mock = createMockCentaurFetch('PONG');
    const thread = createThread();
    const service = new GoogleChatbotService(
      config,
      new InMemoryGoogleChatThreadStateStore(),
      new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
    );

    await service.runChatMessage(thread, chatMessageFixture(), 'execute');
    const requestCount = mock.requests.length;
    await service.runChatMessage(thread, chatMessageFixture(), 'execute');

    expect(mock.requests).toHaveLength(requestCount);
    expect(thread.posts).toEqual(['Thinking...']);
  });

  it('recovers stranded render obligations through the Google Chat adapter', async () => {
    const mock = createMockCentaurFetch('Recovered answer');
    const stateStore = new InMemoryGoogleChatThreadStateStore();
    const updates: Array<{ id: string; text: string; threadId: string }> = [];
    await stateStore.setReference(THREAD_ID, {
      isDm: false,
      spaceName: SPACE_NAME,
      threadName: THREAD_NAME,
    });
    await stateStore.set(THREAD_ID, {
      active: true,
      activeExecution: true,
      activeExecutionStartedAt: Date.now(),
      lastEventId: 0,
      renderObligation: {
        afterEventId: 0,
        executionId: 'exec-1',
        message: messageFixture(),
        progressMessageId: `${SPACE_NAME}/messages/POST-1`,
      },
    });
    await stateStore.indexRenderObligation(THREAD_ID, { maxLength: 2000, ttlMs: 60_000 });
    const service = new GoogleChatbotService(
      config,
      stateStore,
      new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
      {
        gchatAdapter: {
          editMessage: async (threadId: string, id: string, message: { markdown?: string }) => {
            updates.push({ id, text: message.markdown ?? '', threadId });
            return { id, raw: {}, threadId };
          },
          postMessage: async (threadId: string, message: { markdown?: string }) => {
            updates.push({ id: 'new-message', text: message.markdown ?? '', threadId });
            return { id: 'new-message', raw: {}, threadId };
          },
          startTyping: async () => undefined,
        } as never,
      },
    );

    await expect(service.recoverRenderObligations()).resolves.toBe(0);

    expect(updates.at(-1)).toMatchObject({
      id: `${SPACE_NAME}/messages/POST-1`,
      text: 'Recovered answer',
      threadId: THREAD_ID,
    });
    await expect(stateStore.get(THREAD_ID)).resolves.toMatchObject({
      activeExecution: false,
      renderObligation: null,
    });
  });

  it('reports deferred recovery when a live render lease is still active', async () => {
    const stateStore = new InMemoryGoogleChatThreadStateStore();
    await stateStore.setReference(THREAD_ID, { isDm: false, spaceName: SPACE_NAME, threadName: THREAD_NAME });
    await stateStore.set(THREAD_ID, {
      active: true,
      activeExecution: false,
      activeExecutionStartedAt: null,
      renderObligation: {
        afterEventId: 0,
        executionId: 'exec-1',
        message: messageFixture(),
        progressMessageId: `${SPACE_NAME}/messages/POST-1`,
      },
    });
    await stateStore.indexRenderObligation(THREAD_ID, { maxLength: 2000, ttlMs: 60_000 });
    const releaseLiveLease = await stateStore.acquireLiveRenderLease(THREAD_ID, 60_000);
    const service = new GoogleChatbotService(config, stateStore);

    await expect(service.recoverRenderObligations()).resolves.toBe(1);
    await releaseLiveLease();
  });

  it('clears render obligations that cannot be recovered without a space reference', async () => {
    const stateStore = new InMemoryGoogleChatThreadStateStore();
    await stateStore.set(THREAD_ID, {
      active: true,
      activeExecution: false,
      activeExecutionStartedAt: null,
      renderObligation: {
        afterEventId: 0,
        executionId: 'exec-1',
        message: messageFixture(),
        progressMessageId: `${SPACE_NAME}/messages/POST-1`,
      },
    });
    await stateStore.indexRenderObligation(THREAD_ID, { maxLength: 2000, ttlMs: 60_000 });
    const service = new GoogleChatbotService(config, stateStore);

    await expect(service.recoverRenderObligations()).resolves.toBe(0);
    await expect(stateStore.get(THREAD_ID)).resolves.toMatchObject({ renderObligation: null });
  });
});

function createThread(input: { state?: GoogleChatThreadState } = {}): Thread<GoogleChatThreadState> & {
  edits: Array<{ id: string; text: string }>;
  posts: string[];
  stateValue: GoogleChatThreadState;
} {
  const posts: string[] = [];
  const edits: Array<{ id: string; text: string }> = [];
  let stateValue = input.state ?? { active: false };
  const thread = {
    id: THREAD_ID,
    edits,
    posts,
    get stateValue() {
      return stateValue;
    },
    get state() {
      return Promise.resolve(structuredClone(stateValue));
    },
    async setState(update: Partial<GoogleChatThreadState>, options?: { replace?: boolean }) {
      stateValue = options?.replace
        ? structuredClone(update as GoogleChatThreadState)
        : { ...stateValue, ...structuredClone(update) };
    },
    async post(message: string | AsyncIterable<string>) {
      if (typeof message === 'string') {
        posts.push(message);
      } else {
        let text = '';
        for await (const chunk of message) {
          text += chunk;
        }
        posts.push(text);
      }
      return { id: `${SPACE_NAME}/messages/POST-${posts.length}`, raw: {}, threadId: THREAD_ID };
    },
    async startTyping() {},
    adapter: {
      async editMessage(_threadId: string, id: string, message: { markdown?: string }) {
        edits.push({ id, text: message.markdown ?? '' });
        return { id, raw: {}, threadId: THREAD_ID };
      },
      async postMessage(_threadId: string, message: { markdown?: string }) {
        posts.push(message.markdown ?? '');
        return { id: `${SPACE_NAME}/messages/POST-${posts.length}`, raw: {}, threadId: THREAD_ID };
      },
    },
  };
  return thread as never;
}

function chatMessageFixture(input: {
  event?: GoogleChatEventEnvelope;
  id?: string;
  isMention?: boolean;
  text?: string;
} = {}) {
  const event = input.event ?? eventFixture(input.id ? { name: input.id } : {});
  return {
    attachments: [],
    id: input.id ?? MESSAGE_ID,
    isMention: input.isMention ?? true,
    raw: event,
    text: input.text ?? 'Reply exactly PONG.',
  } as never;
}

function eventFixture(messageOverrides: Record<string, unknown> = {}): GoogleChatEventEnvelope {
  return {
    chat: {
      messagePayload: {
        message: {
          createTime: '2026-06-22T12:00:00.000Z',
          name: MESSAGE_ID,
          sender: { displayName: 'Casey', email: 'casey@example.com', name: 'users/123', type: 'HUMAN' },
          text: '@centaur Reply exactly PONG.',
          thread: { name: THREAD_NAME },
          ...messageOverrides,
        },
        space: { displayName: 'Centaur Demo', name: SPACE_NAME, spaceType: 'SPACE' },
      },
      user: { displayName: 'Casey', email: 'casey@example.com', name: 'users/123', type: 'HUMAN' },
    },
  };
}

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
    id: MESSAGE_ID,
    isMention: true,
    raw: {},
    spaceDisplayName: 'Centaur Demo',
    spaceName: SPACE_NAME,
    spaceType: 'SPACE',
    text: 'Reply exactly PONG.',
    threadId: THREAD_ID,
    threadName: THREAD_NAME,
    timestamp: '2026-06-22T12:00:00.000Z',
    ...overrides,
  };
}

async function waitFor(predicate: () => boolean, timeoutMs = 250): Promise<void> {
  const startedAt = Date.now();
  while (!predicate()) {
    if (Date.now() - startedAt > timeoutMs) {
      throw new Error('timed out waiting for condition');
    }
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
}
