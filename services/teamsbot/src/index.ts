import { Readable } from 'node:stream';
import { createTeamsAdapter, type TeamsAdapter } from '@chat-adapter/teams';
import {
  Chat,
  type Logger,
  type StateAdapter,
} from 'chat';
import express, { type Express, type Request as ExpressRequest, type Response as ExpressResponse } from 'express';
import type { TeamsbotConfig } from './config.js';
import { createTeamsbotLogger } from './logger.js';
import { PostgresTeamsbotStateStore } from './state.js';
import { TeamsbotService } from './teamsbot.js';
import type { TeamsThreadState, TeamsThreadStateStore } from './types.js';

const RECOVERY_RETRY_INITIAL_DELAY_MS = 250;
const RECOVERY_RETRY_MAX_DELAY_MS = 5_000;

export type TeamsbotOptions = {
  config: TeamsbotConfig;
  logger?: Logger;
  state?: StateAdapter;
  stateStore?: TeamsThreadStateStore;
  teamsAdapter?: TeamsAdapter;
};

type RenderRecoveryScheduler = {
  schedule(delayMs?: number): void;
};

export type TeamsbotInstance = {
  app: Express;
  chat: Chat<{ teams: TeamsAdapter }, TeamsThreadState>;
  isReady(): boolean;
  logger: Logger;
  start(): ReturnType<Express['listen']>;
  stateStore: TeamsThreadStateStore;
  teamsAdapter: TeamsAdapter;
  teamsbot: TeamsbotService;
};

export async function createTeamsbot(options: TeamsbotOptions): Promise<TeamsbotInstance> {
  const { config } = options;
  requireTeamsCredential(config.teams.appId, 'TEAMS_BOT_APP_ID');
  requireTeamsCredential(config.teams.appPassword, 'TEAMS_BOT_APP_PASSWORD');
  requireTeamsCredential(config.teams.appTenantId, 'TEAMS_BOT_APP_TENANT_ID');

  const app = express();
  const logger = options.logger ?? createTeamsbotLogger(config.server.logLevel);
  const stateStore = options.stateStore ?? createStateStore(config, logger.child('postgres-state'));
  const state = options.state ?? stateAdapterForStore(stateStore);
  const teamsAdapter = options.teamsAdapter ?? createTeamsAdapter({
    appId: config.teams.appId,
    appPassword: config.teams.appPassword,
    appTenantId: config.teams.appTenantId,
    appType: 'SingleTenant',
    logger,
    userName: 'centaur',
  });
  const chat = new Chat<{ teams: TeamsAdapter }, TeamsThreadState>({
    userName: 'centaur',
    adapters: { teams: teamsAdapter },
    state,
    concurrency: 'concurrent',
    fallbackStreamingPlaceholderText: null,
    logger,
  });
  let ready = false;
  let renderRecoveryScheduler: RenderRecoveryScheduler | undefined;

  const teamsbot = new TeamsbotService(config, stateStore, undefined, {
    logger,
    onRenderObligationIndexed: () => renderRecoveryScheduler?.schedule(),
    teamsAdapter,
  });
  renderRecoveryScheduler = createRenderRecoveryScheduler(teamsbot);

  chat.onDirectMessage(async (thread, message) => {
    await thread.subscribe();
    await teamsbot.runChatMessage(thread, message, 'execute');
  });

  chat.onNewMention(async (thread, message) => {
    await thread.subscribe();
    await teamsbot.runChatMessage(thread, message, 'execute');
  });

  chat.onSubscribedMessage(async (thread, message) => {
    await teamsbot.runChatMessage(thread, message, message.isMention === true ? 'execute' : 'append');
  });

  app.get('/live', (_request, response) => {
    response.json({ ok: true, service: 'teamsbot' });
  });

  app.get(['/health', '/ready'], (_request, response) => {
    response.status(ready ? 200 : 503).json({ ok: ready, service: 'teamsbot' });
  });

  app.post('/api/messages', async (request, response, next) => {
    try {
      const webhookResponse = await chat.webhooks.teams(toWebRequest(request), {
        waitUntil: (task) => {
          void task.catch((error) => {
            logger.error('teamsbot_webhook_task_failed', {
              error: error instanceof Error ? error.message : String(error),
            });
          });
        },
      });
      await writeWebResponse(response, webhookResponse);
    } catch (error) {
      next(error);
    }
  });

  return {
    app,
    chat,
    isReady: () => ready,
    start() {
      return app.listen(config.server.port, '0.0.0.0', () => {
        logger.info('teamsbot_started', { port: config.server.port });
        void ensureStateConnected(state, logger)
          .then(() => chat.initialize())
          .then(() => {
            ready = true;
            renderRecoveryScheduler.schedule();
          })
          .catch((error) => {
            logger.error('teamsbot_startup_initialization_failed', { error });
            process.exitCode = 1;
            setTimeout(() => process.exit(1), 100);
          });
      });
    },
    logger,
    stateStore,
    teamsAdapter,
    teamsbot,
  };
}

function createStateStore(config: TeamsbotConfig, logger: Logger): PostgresTeamsbotStateStore {
  if (!config.server.postgresUrl) {
    throw new Error('TEAMSBOT_DATABASE_URL (or DATABASE_URL / POSTGRES_URL) is required');
  }
  return new PostgresTeamsbotStateStore({
    logger,
    postgresUrl: config.server.postgresUrl,
    stateKeyPrefix: config.server.stateKeyPrefix,
  });
}

async function ensureStateConnected(state: StateAdapter, logger: Logger): Promise<void> {
  for (let attempt = 0; ; attempt++) {
    try {
      await state.connect();
      return;
    } catch (error) {
      const delayMs = Math.min(250 * 2 ** attempt, 10_000);
      logger.warn('teamsbot_postgres_connect_retry', {
        attempt: attempt + 1,
        delayMs,
        error: error instanceof Error ? error.message : String(error),
      });
      await sleep(delayMs);
    }
  }
}

function stateAdapterForStore(stateStore: TeamsThreadStateStore): StateAdapter {
  if (stateStore instanceof PostgresTeamsbotStateStore) {
    return stateStore.adapter;
  }
  throw new Error('A Chat SDK StateAdapter is required when using a custom Teamsbot state store');
}

export function createRenderRecoveryScheduler(teamsbot: Pick<TeamsbotService, 'logger' | 'recoverRenderObligations'>): RenderRecoveryScheduler {
  let scheduledTimer: ReturnType<typeof setTimeout> | undefined;
  let scheduledDelayMs: number | undefined;
  let running = false;
  let rescheduleRequested = false;
  let attempt = 0;

  function schedule(delayMs = 0): void {
    if (running) {
      rescheduleRequested = true;
      return;
    }
    if (scheduledTimer) {
      if (scheduledDelayMs !== undefined && scheduledDelayMs <= delayMs) {
        return;
      }
      clearTimeout(scheduledTimer);
    }
    scheduledDelayMs = delayMs;
    scheduledTimer = setTimeout(() => {
      scheduledTimer = undefined;
      scheduledDelayMs = undefined;
      void run();
    }, delayMs);
  }

  async function run(): Promise<void> {
    if (running) {
      rescheduleRequested = true;
      return;
    }
    running = true;
    let retryDelayMs: number | undefined;
    try {
      const deferredCount = await teamsbot.recoverRenderObligations();
      if (deferredCount === 0) {
        attempt = 0;
        return;
      }
      retryDelayMs = Math.min(RECOVERY_RETRY_INITIAL_DELAY_MS * 2 ** attempt, RECOVERY_RETRY_MAX_DELAY_MS);
      attempt += 1;
      teamsbot.logger.warn('teamsbot_render_recovery_retry_scheduled', {
        deferredCount,
        delayMs: retryDelayMs,
        attempt,
      });
    } catch (error) {
      teamsbot.logger.error('teamsbot_render_recovery_loop_failed', { error });
      retryDelayMs = RECOVERY_RETRY_MAX_DELAY_MS;
    } finally {
      running = false;
      if (rescheduleRequested) {
        rescheduleRequested = false;
        attempt = 0;
        schedule();
      } else if (retryDelayMs !== undefined) {
        schedule(retryDelayMs);
      }
    }
  }

  return { schedule };
}

function toWebRequest(request: ExpressRequest): Request {
  const headers = new Headers();
  for (const [key, value] of Object.entries(request.headers)) {
    if (Array.isArray(value)) {
      for (const entry of value) {
        headers.append(key, entry);
      }
    } else if (value !== undefined) {
      headers.set(key, String(value));
    }
  }
  const protocol = request.protocol || 'http';
  const host = request.headers.host ?? `localhost`;
  const url = `${protocol}://${host}${request.originalUrl || request.url}`;
  const method = request.method.toUpperCase();
  const body = method === 'GET' || method === 'HEAD'
    ? undefined
    : (Readable.toWeb(request) as unknown as ReadableStream<Uint8Array>);
  return new Request(url, {
    method,
    headers,
    body,
    ...(body ? { duplex: 'half' as const } : {}),
  } as RequestInit & { duplex?: 'half' });
}

async function writeWebResponse(response: ExpressResponse, webResponse: Response): Promise<void> {
  response.status(webResponse.status);
  webResponse.headers.forEach((value, key) => {
    response.setHeader(key, value);
  });
  response.send(Buffer.from(await webResponse.arrayBuffer()));
}

function requireTeamsCredential(value: string, envName: string): void {
  if (!value.trim()) {
    throw new Error(`${envName} is required`);
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
