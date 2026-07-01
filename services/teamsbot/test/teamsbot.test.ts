import { describe, expect, it } from 'bun:test';
import type { Thread } from 'chat';
import type { TeamsbotConfig } from '../src/config.js';
import { createRenderRecoveryScheduler, createTeamsbot } from '../src/index.js';
import { CentaurSessionClient } from '../src/session-api.js';
import { hasLiveActiveExecution, TeamsbotService } from '../src/teamsbot.js';
import { isAllowedTeamsActivity, serializeTeamsMessage } from '../src/teams-message.js';
import type { TeamsActivity, TeamsApiMessage, TeamsThreadState } from '../src/types.js';
import { InMemoryTeamsThreadStateStore } from './support/in-memory-state.js';
import { createMockCentaurFetch } from './support/mock-centaur.js';

const THREAD_ID = 'teams:Y29udmVyc2F0aW9uLTE:aHR0cHM6Ly9zbWJhLnRyYWZmaWNtYW5hZ2VyLm5ldC9hbWVyLw';

const config: TeamsbotConfig = {
  centaur: { apiUrl: 'http://mock-centaur.local', requestMaxRetries: 0, requestRetryDelayMs: 0 },
  server: { logLevel: 'silent', port: 0 },
  teams: {
    allowedChannelIds: [],
    allowedTeamIds: ['team-1'],
    allowedTenantIds: ['tenant-1'],
    appId: 'bot-id',
    appPassword: 'bot-password',
    appTenantId: 'tenant-1',
    attachmentAllowedHosts: ['files.example', 'graph.microsoft.com'],
    attachmentDownloadEnabled: false,
    attachmentMaxBytes: 1024 * 1024,
    activeExecutionTtlMs: 30 * 60 * 1000,
    defaultHarnessType: 'codex',
    graphTokenScope: 'https://graph.microsoft.com/.default',
    renderDeliveryTimeoutMs: 15_000,
    requireMention: true,
  },
};

describe('TeamsbotService', () => {
  it('requires Bot Framework credentials when creating the Teams adapter', async () => {
    await expect(createTeamsbot({
      config: {
        ...config,
        teams: { ...config.teams, appPassword: '' },
      },
      stateStore: new InMemoryTeamsThreadStateStore(),
    })).rejects.toThrow('TEAMS_BOT_APP_PASSWORD is required');
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

  it('fails closed for non-Teams activities and empty allowlists', () => {
    expect(isAllowedTeamsActivity({
      activity: { channelId: 'webchat' },
      allowedChannelIds: [],
      allowedTeamIds: [],
      allowedTenantIds: [],
    })).toBe(false);
    expect(isAllowedTeamsActivity({
      activity: {
        channelData: { team: { id: 'team-1' }, channel: { id: 'channel-1' } },
        channelId: 'msteams',
      },
      allowedChannelIds: [],
      allowedTeamIds: [],
      allowedTenantIds: [],
    })).toBe(false);
  });

  it('allows configured Teams channel and personal-chat scopes', () => {
    expect(isAllowedTeamsActivity({
      activity: activityFixture(),
      allowedChannelIds: [],
      allowedTeamIds: ['team-1'],
      allowedTenantIds: ['tenant-1'],
    })).toBe(true);
    expect(isAllowedTeamsActivity({
      activity: activityFixture({
        channelData: { tenant: { id: 'tenant-1' } },
        conversation: { id: 'conversation-1', conversationType: 'personal', tenantId: 'tenant-1' },
      }),
      allowedChannelIds: [],
      allowedTeamIds: [],
      allowedTenantIds: ['tenant-1'],
    })).toBe(true);
  });

  it('uses tenant allowlists as an outer boundary for channel messages', () => {
    expect(isAllowedTeamsActivity({
      activity: activityFixture({
        channelData: {
          channel: { id: 'channel-1' },
          team: { id: 'team-1' },
          tenant: { id: 'tenant-2' },
        },
        conversation: { id: 'conversation-1', conversationType: 'channel', tenantId: 'tenant-2' },
      }),
      allowedChannelIds: [],
      allowedTeamIds: ['team-1'],
      allowedTenantIds: ['tenant-1'],
    })).toBe(false);
  });

  it('denies non-team group chats even when the tenant is allowed', () => {
    expect(isAllowedTeamsActivity({
      activity: activityFixture({
        channelData: { tenant: { id: 'tenant-1' } },
        conversation: { id: 'conversation-1', conversationType: 'groupChat', tenantId: 'tenant-1' },
      }),
      allowedChannelIds: [],
      allowedTeamIds: [],
      allowedTenantIds: ['tenant-1'],
    })).toBe(false);
  });

  it('redacts Teams attachment URLs from serialized raw activity', () => {
    const message = serializeTeamsMessage({ activity: activityFixture({
      attachments: [{
        content: {
          downloadUrl: 'https://files.example/people.csv?sig=secret',
          fileName: 'people.csv',
          fileType: 'csv',
        },
        contentType: 'application/vnd.microsoft.teams.file.download.info',
        name: 'people.csv',
      }],
      conversation: { id: 'conversation-1', conversationType: 'personal', tenantId: 'tenant-1' },
    }) }, THREAD_ID, 'check this');

    expect(message.attachments[0]?.content).toEqual({
      downloadUrlRedacted: true,
      fileName: 'people.csv',
      fileType: 'csv',
    });
    expect(JSON.stringify(message.raw)).not.toContain('https://files.example');
    expect(JSON.stringify(message.raw)).toContain('downloadUrlRedacted');
  });

  it('redacts regular attachment content URLs from raw activity', () => {
    const message = serializeTeamsMessage({ activity: activityFixture({
      attachments: [{
        contentType: 'text/csv',
        contentUrl: 'https://files.example/people.csv?sig=secret',
        name: 'people.csv',
      }],
      conversation: { id: 'conversation-1', conversationType: 'personal', tenantId: 'tenant-1' },
    }) }, THREAD_ID, 'check this');

    expect(message.attachments[0]?.contentUrl).toBe('https://files.example/people.csv?sig=secret');
    expect(JSON.stringify(message.raw)).not.toContain('https://files.example');
    expect(JSON.stringify(message.raw)).toContain('contentUrlRedacted');
  });

  it('treats only timestamped in-flight executions inside the TTL as live', () => {
    expect(hasLiveActiveExecution({ active: true, activeExecution: true, activeExecutionStartedAt: 1_000 }, 1_000, 1_500)).toBe(true);
    expect(hasLiveActiveExecution({ active: true, activeExecution: true, activeExecutionStartedAt: 1_000 }, 1_000, 2_001)).toBe(false);
    expect(hasLiveActiveExecution({ active: true, activeExecution: true }, 1_000, 1_500)).toBe(false);
  });

  it('executes a mentioned Teams message through the official Chat SDK thread id', async () => {
    const mock = createMockCentaurFetch('PONG');
    const thread = createThread({ conversationType: 'channel' });
    const service = new TeamsbotService(
      config,
      new InMemoryTeamsThreadStateStore(),
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
    expect(thread.edits).toEqual([{ id: 'activity-1', text: 'PONG' }]);
  });

  it('stores the Teams conversation reference for adapter-backed recovery', async () => {
    const mock = createMockCentaurFetch('PONG');
    const stateStore = new InMemoryTeamsThreadStateStore();
    const thread = createThread({ conversationType: 'channel' });
    const service = new TeamsbotService(
      config,
      stateStore,
      new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
    );

    await service.runChatMessage(thread, chatMessageFixture(), 'execute');

    await expect(stateStore.getReference(THREAD_ID)).resolves.toMatchObject({
      activityId: 'message-1',
      channelId: 'msteams',
      conversationId: 'conversation-1',
      serviceUrl: 'https://smba.trafficmanager.net/amer/',
      teamId: 'team-1',
      tenantId: 'tenant-1',
    });
  });

  it('sends quoted Teams thread context with executions', async () => {
    const mock = createMockCentaurFetch('PONG');
    const thread = createThread({ conversationType: 'channel' });
    const service = new TeamsbotService(
      config,
      new InMemoryTeamsThreadStateStore(),
      new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
    );

    await service.runChatMessage(thread, chatMessageFixture({
      activity: activityFixture({
        entities: [{
          quotedReply: {
            messageId: 'message-1',
            preview: 'Original request: summarize this pipeline.',
            senderId: 'user-2',
            senderName: 'Riley',
            time: '1772050244572',
          },
          type: 'quotedreply',
        }],
        id: 'message-2',
        replyToId: 'message-1',
      }),
      id: 'message-2',
      text: 'please do this',
    }), 'execute');

    const executeBody = mock.requests.find((request) => request.path.endsWith('/execute'))?.body as { input_lines: string[] };
    const inputText = JSON.stringify(JSON.parse(executeBody.input_lines.at(-1)!));
    expect(inputText).toContain('Teams Thread Context');
    expect(inputText).toContain('Riley');
    expect(inputText).toContain('Original request: summarize this pipeline.');
  });

  it('keeps channel block replies at Thinking until completion', async () => {
    const mock = createMockCentaurFetch('PONG', ['PO', 'NG']);
    const thread = createThread({ conversationType: 'channel' });
    const service = new TeamsbotService(
      config,
      new InMemoryTeamsThreadStateStore(),
      new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
    );

    await service.runChatMessage(thread, chatMessageFixture(), 'execute');

    expect(thread.posts).toEqual(['Thinking...']);
    expect(thread.edits).toEqual([{ id: 'activity-1', text: 'PONG' }]);
  });

  it('streams personal-chat replies after a visible Thinking placeholder', async () => {
    const mock = createMockCentaurFetch('PONG', ['P', 'O', 'NG'], { chunkDelayMs: 550 });
    const thread = createThread({ conversationType: 'personal' });
    const service = new TeamsbotService(
      { ...config, teams: { ...config.teams, allowedTeamIds: [], allowedTenantIds: ['tenant-1'] } },
      new InMemoryTeamsThreadStateStore(),
      new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
    );

    await service.runChatMessage(
      thread,
      chatMessageFixture({ activity: activityFixture({ conversation: { id: 'conversation-1', conversationType: 'personal', tenantId: 'tenant-1' } }) }),
      'execute',
    );

    expect(thread.posts).toEqual(['Thinking...']);
    expect(thread.edits).toEqual([
      { id: 'activity-1', text: 'PO' },
      { id: 'activity-1', text: 'PONG' },
    ]);
  });

  it('preserves Teams attachment metadata in forwarded session messages', async () => {
    const mock = createMockCentaurFetch('PONG');
    const thread = createThread({ conversationType: 'channel' });
    const service = new TeamsbotService(
      config,
      new InMemoryTeamsThreadStateStore(),
      new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
    );

    await service.runChatMessage(thread, chatMessageFixture({
      activity: activityFixture({
        attachments: [{
          contentType: 'text/csv',
          contentUrl: 'https://files.example/people.csv?sig=secret',
          name: 'people.csv',
        }],
      }),
    }), 'execute');

    const messagesBody = mock.requests.find((request) => request.path.endsWith('/messages'))?.body as {
      messages: Array<{ parts: unknown[] }>;
    };
    expect(messagesBody.messages[0]?.parts).toContainEqual({
      attachment_type: 'teams',
      contentType: 'text/csv',
      mimeType: 'text/csv',
      name: 'people.csv',
      type: 'attachment',
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
          progressActivityId: 'activity-1',
        },
      },
    });
    const service = new TeamsbotService(
      config,
      new InMemoryTeamsThreadStateStore(),
      new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
    );

    await service.runChatMessage(thread, chatMessageFixture({ id: 'message-2', text: 'additional context', isMention: false }), 'append');

    expect(mock.requests.map((request) => `${request.method} ${request.path}`)).toEqual([
      `POST /api/session/${encodeURIComponent(THREAD_ID)}`,
      `POST /api/session/${encodeURIComponent(THREAD_ID)}/messages`,
    ]);
    expect(thread.stateValue.forwardedMessageIds).toEqual(['message-2']);
    expect(thread.posts).toEqual([]);
  });

  it('ignores duplicate activity redelivery after the message executed', async () => {
    const mock = createMockCentaurFetch('PONG');
    const thread = createThread({ conversationType: 'channel' });
    const service = new TeamsbotService(
      config,
      new InMemoryTeamsThreadStateStore(),
      new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
    );

    await service.runChatMessage(thread, chatMessageFixture(), 'execute');
    const requestCount = mock.requests.length;
    await service.runChatMessage(thread, chatMessageFixture(), 'execute');

    expect(mock.requests).toHaveLength(requestCount);
    expect(thread.posts).toEqual(['Thinking...']);
  });

  it('recovers stranded render obligations through the Teams adapter', async () => {
    const mock = createMockCentaurFetch('Recovered answer');
    const stateStore = new InMemoryTeamsThreadStateStore();
    const updates: Array<{ id: string; text: string; threadId: string }> = [];
    await stateStore.setReference(THREAD_ID, {
      activityId: 'message-1',
      channelId: 'msteams',
      conversationId: 'conversation-1',
      conversationType: 'channel',
      serviceUrl: 'https://smba.trafficmanager.net/amer/',
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
        progressActivityId: 'activity-1',
      },
    });
    await stateStore.indexRenderObligation(THREAD_ID, { maxLength: 2000, ttlMs: 60_000 });
    const service = new TeamsbotService(
      config,
      stateStore,
      new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
      {
        teamsAdapter: {
          editMessage: async (threadId: string, id: string, message: { markdown?: string }) => {
            updates.push({ id, text: message.markdown ?? '', threadId });
            return { id, raw: {}, threadId };
          },
          postMessage: async (threadId: string, message: { markdown?: string }) => {
            updates.push({ id: 'new-activity', text: message.markdown ?? '', threadId });
            return { id: 'new-activity', raw: {}, threadId };
          },
          startTyping: async () => undefined,
        } as never,
      },
    );

    await expect(service.recoverRenderObligations()).resolves.toBe(0);

    expect(updates.at(-1)).toMatchObject({ id: 'activity-1', text: 'Recovered answer', threadId: THREAD_ID });
    await expect(stateStore.get(THREAD_ID)).resolves.toMatchObject({
      activeExecution: false,
      renderObligation: null,
    });
  });

  it('reports deferred recovery when a live render lease is still active', async () => {
    const stateStore = new InMemoryTeamsThreadStateStore();
    await stateStore.setReference(THREAD_ID, {
      activityId: 'message-1',
      channelId: 'msteams',
      conversationId: 'conversation-1',
      conversationType: 'channel',
      serviceUrl: 'https://smba.trafficmanager.net/amer/',
    });
    await stateStore.set(THREAD_ID, {
      active: true,
      activeExecution: false,
      activeExecutionStartedAt: null,
      renderObligation: {
        afterEventId: 0,
        executionId: 'exec-1',
        message: messageFixture(),
        progressActivityId: 'activity-1',
      },
    });
    await stateStore.indexRenderObligation(THREAD_ID, { maxLength: 2000, ttlMs: 60_000 });
    const releaseLiveLease = await stateStore.acquireLiveRenderLease(THREAD_ID, 60_000);
    const service = new TeamsbotService(config, stateStore);

    await expect(service.recoverRenderObligations()).resolves.toBe(1);
    await releaseLiveLease();
  });

  it('clears render obligations that cannot be recovered without a conversation reference', async () => {
    const stateStore = new InMemoryTeamsThreadStateStore();
    await stateStore.set(THREAD_ID, {
      active: true,
      activeExecution: false,
      activeExecutionStartedAt: null,
      renderObligation: {
        afterEventId: 0,
        executionId: 'exec-1',
        message: messageFixture(),
        progressActivityId: 'activity-1',
      },
    });
    await stateStore.indexRenderObligation(THREAD_ID, { maxLength: 2000, ttlMs: 60_000 });
    const service = new TeamsbotService(config, stateStore);

    await expect(service.recoverRenderObligations()).resolves.toBe(0);
    await expect(stateStore.get(THREAD_ID)).resolves.toMatchObject({ renderObligation: null });
  });
});

function createThread(input: {
  conversationType?: string;
  state?: TeamsThreadState;
} = {}): Thread<TeamsThreadState> & {
  edits: Array<{ id: string; text: string }>;
  posts: string[];
  stateValue: TeamsThreadState;
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
    async setState(update: Partial<TeamsThreadState>, options?: { replace?: boolean }) {
      stateValue = options?.replace ? structuredClone(update as TeamsThreadState) : { ...stateValue, ...structuredClone(update) };
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
      return { id: `activity-${posts.length}`, raw: {}, threadId: THREAD_ID };
    },
    async startTyping() {},
    adapter: {
      async editMessage(_threadId: string, id: string, message: { markdown?: string }) {
        edits.push({ id, text: message.markdown ?? '' });
        return { id, raw: {}, threadId: THREAD_ID };
      },
      async postMessage(_threadId: string, message: { markdown?: string }) {
        posts.push(message.markdown ?? '');
        return { id: `activity-${posts.length}`, raw: {}, threadId: THREAD_ID };
      },
    },
  };
  return thread as never;
}

function chatMessageFixture(input: {
  activity?: TeamsActivity;
  id?: string;
  isMention?: boolean;
  text?: string;
} = {}) {
  const activity = input.activity ?? activityFixture({
    ...(input.id ? { id: input.id } : {}),
    text: `<at>Centaur</at> ${input.text ?? 'Reply exactly PONG.'}`,
  });
  return {
    id: input.id ?? activity.id ?? 'message-1',
    isMention: input.isMention ?? true,
    raw: activity,
    text: input.text ?? 'Reply exactly PONG.',
  } as never;
}

function activityFixture(overrides: Partial<TeamsActivity> = {}): TeamsActivity {
  return {
    channelData: {
      channel: { id: 'channel-1' },
      team: { id: 'team-1' },
      tenant: { id: 'tenant-1' },
    },
    channelId: 'msteams',
    conversation: { id: 'conversation-1', conversationType: 'channel', tenantId: 'tenant-1' },
    from: { aadObjectId: 'aad-user-1', id: 'user-1', name: 'Casey' },
    id: 'message-1',
    recipient: { id: 'bot-id', name: 'Centaur' },
    serviceUrl: 'https://smba.trafficmanager.net/amer/',
    text: '<at>Centaur</at> Reply exactly PONG.',
    timestamp: '2026-06-22T12:00:00.000Z',
    type: 'message',
    ...overrides,
  };
}

function messageFixture(): TeamsApiMessage {
  return {
    attachments: [],
    author: {
      aadObjectId: 'aad-user-1',
      fullName: 'Casey',
      isBot: false,
      userId: 'user-1',
      userName: 'Casey',
    },
    channelId: 'channel-1',
    conversationId: 'conversation-1',
    conversationType: 'channel',
    id: 'message-1',
    isMention: true,
    raw: {},
    teamId: 'team-1',
    tenantId: 'tenant-1',
    text: 'Reply exactly PONG.',
    threadId: THREAD_ID,
    timestamp: '2026-06-22T12:00:00.000Z',
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
