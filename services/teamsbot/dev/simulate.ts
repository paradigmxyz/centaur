import { loadConfig } from '../src/config.js';
import { createMockCentaurFetch } from '../test/support/mock-centaur.js';
import { CentaurSessionClient } from '../src/session-api.js';
import { InMemoryTeamsThreadStateStore } from '../test/support/in-memory-state.js';
import { TeamsbotService } from '../src/teamsbot.js';
import type { TeamsbotConfig } from '../src/config.js';
import type { Thread } from 'chat';
import type { TeamsThreadState } from '../src/types.js';

const text = process.argv.slice(2).join(' ') || 'Reply exactly PONG.';
const loaded = loadConfig();
const threadId = 'teams:Y29udmVyc2F0aW9uLTE:aHR0cHM6Ly9zbWJhLnRyYWZmaWNtYW5hZ2VyLm5ldC9hbWVyLw';
const config: TeamsbotConfig = {
  ...loaded,
  centaur: { apiUrl: 'http://mock-centaur.local', requestMaxRetries: 0, requestRetryDelayMs: 0 },
  teams: { ...loaded.teams, allowedTeamIds: ['team-1'], allowedTenantIds: ['tenant-1'] },
};
const mock = createMockCentaurFetch('PONG');
const stateStore = new InMemoryTeamsThreadStateStore();
const service = new TeamsbotService(
  config,
  stateStore,
  new CentaurSessionClient({ apiUrl: config.centaur.apiUrl, fetch: mock.fetch }),
);
const thread = createThread(threadId);
const activity = {
  type: 'message',
  channelId: 'msteams',
  id: 'message-1',
  text: `<at>Centaur</at> ${text}`,
  recipient: { id: 'bot-id', name: 'Centaur' },
  from: { id: 'user-1', name: 'Casey' },
  conversation: { id: 'conversation-1', conversationType: 'channel', tenantId: 'tenant-1' },
  serviceUrl: 'https://smba.trafficmanager.net/amer/',
  channelData: { team: { id: 'team-1' }, channel: { id: 'channel-1' }, tenant: { id: 'tenant-1' } },
  entities: [{ type: 'mention', mentioned: { id: 'bot-id', name: 'Centaur' }, text: '<at>Centaur</at>' }],
};

await service.runChatMessage(thread, {
  id: 'message-1',
  isMention: true,
  raw: activity,
  text,
} as never, 'execute');

console.log(JSON.stringify({
  edits: thread.edits,
  posts: thread.posts,
  requests: mock.requests,
  state: await stateStore.list(),
}, null, 2));

function createThread(id: string): Thread<TeamsThreadState> & {
  edits: Array<{ id: string; text: string }>;
  posts: string[];
} {
  const edits: Array<{ id: string; text: string }> = [];
  const posts: string[] = [];
  let state: TeamsThreadState = { active: false };
  return {
    id,
    edits,
    posts,
    get state() {
      return Promise.resolve(structuredClone(state));
    },
    async setState(update: Partial<TeamsThreadState>, options?: { replace?: boolean }) {
      state = options?.replace ? structuredClone(update as TeamsThreadState) : { ...state, ...structuredClone(update) };
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
      return { id: `activity-${posts.length}`, raw: {}, threadId: id };
    },
    async startTyping() {},
    adapter: {
      async editMessage(_threadId: string, messageId: string, message: { markdown?: string }) {
        edits.push({ id: messageId, text: message.markdown ?? '' });
        return { id: messageId, raw: {}, threadId: id };
      },
      async postMessage(_threadId: string, message: { markdown?: string }) {
        posts.push(message.markdown ?? '');
        return { id: `activity-${posts.length}`, raw: {}, threadId: id };
      },
    },
  } as never;
}
