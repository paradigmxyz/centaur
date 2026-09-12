import type { Thread } from 'chat';
import { loadConfig, type GoogleChatbotConfig } from '../src/config.js';
import { GoogleChatbotService } from '../src/googlechatbot.js';
import { CentaurSessionClient } from '../src/session-api.js';
import type { GoogleChatThreadState } from '../src/types.js';
import { InMemoryGoogleChatThreadStateStore } from '../test/support/in-memory-state.js';
import { createMockCentaurFetch } from '../test/support/mock-centaur.js';

const text = process.argv.slice(2).join(' ') || 'Reply exactly PONG.';
const loaded = loadConfig({ ...process.env, GOOGLE_CHAT_USE_ADC: 'true', GOOGLE_CHAT_PROJECT_NUMBER: '1234567890' });
const threadKey = 'gchat:spaces/AAAA1111:c3BhY2VzL0FBQUExMTExL3RocmVhZHMvVFRUVA';
const config: GoogleChatbotConfig = {
  ...loaded,
  centaur: { apiUrl: 'http://mock-centaur.local', requestMaxRetries: 0, requestRetryDelayMs: 0 },
  gchat: {
    ...loaded.gchat,
    allowedSpaceIds: ['spaces/AAAA1111'],
    renderMinEditIntervalMs: 0,
  },
};
const mock = createMockCentaurFetch('PONG');
const stateStore = new InMemoryGoogleChatThreadStateStore();
const service = new GoogleChatbotService(
  config,
  stateStore,
  new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
);
const thread = createThread(threadKey);
const event = {
  chat: {
    messagePayload: {
      message: {
        name: 'spaces/AAAA1111/messages/MMMM',
        createTime: '2026-01-01T00:00:00Z',
        text: `@centaur ${text}`,
        sender: { name: 'users/123', displayName: 'Casey', email: 'casey@example.com', type: 'HUMAN' },
        thread: { name: 'spaces/AAAA1111/threads/TTTT' },
      },
      space: { name: 'spaces/AAAA1111', spaceType: 'SPACE', displayName: 'Centaur Demo' },
    },
  },
};

await service.runChatMessage(thread, {
  attachments: [],
  id: 'spaces/AAAA1111/messages/MMMM',
  isMention: true,
  raw: event,
  text,
} as never, 'execute');

console.log(JSON.stringify({
  edits: thread.edits,
  posts: thread.posts,
  requests: mock.requests,
  state: await stateStore.list(),
}, null, 2));

function createThread(id: string): Thread<GoogleChatThreadState> & {
  edits: Array<{ id: string; text: string }>;
  posts: string[];
} {
  const edits: Array<{ id: string; text: string }> = [];
  const posts: string[] = [];
  let state: GoogleChatThreadState = { active: false };
  return {
    id,
    edits,
    posts,
    get state() {
      return Promise.resolve(structuredClone(state));
    },
    async setState(update: Partial<GoogleChatThreadState>, options?: { replace?: boolean }) {
      state = options?.replace ? structuredClone(update as GoogleChatThreadState) : { ...state, ...structuredClone(update) };
    },
    async post(message: string | AsyncIterable<string>) {
      if (typeof message === 'string') {
        posts.push(message);
      } else {
        let body = '';
        for await (const chunk of message) {
          body += chunk;
        }
        posts.push(body);
      }
      return { id: `spaces/AAAA1111/messages/POST-${posts.length}`, raw: {}, threadId: id };
    },
    async startTyping() {},
    adapter: {
      async editMessage(_threadId: string, messageId: string, message: { markdown?: string }) {
        edits.push({ id: messageId, text: message.markdown ?? '' });
        return { id: messageId, raw: {}, threadId: id };
      },
      async postMessage(_threadId: string, message: { markdown?: string }) {
        posts.push(message.markdown ?? '');
        return { id: `spaces/AAAA1111/messages/POST-${posts.length}`, raw: {}, threadId: id };
      },
    },
  } as never;
}
