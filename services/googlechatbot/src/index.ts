import { createGoogleChatAdapter, type GoogleChatAdapter } from '@chat-adapter/gchat';
import { Chat, type Logger, type StateAdapter } from 'chat';
import { Hono } from 'hono';
import {
  assertGoogleChatCredentials,
  assertGoogleChatPubsubVerification,
  assertGoogleChatWebhookVerification,
  type GoogleChatbotConfig,
} from './config.js';
import { GoogleChatbotService } from './googlechatbot.js';
import { createGoogleChatbotLogger } from './logger.js';
import { PostgresGoogleChatbotStateStore } from './state.js';
import type { GoogleChatThreadState, GoogleChatThreadStateStore } from './types.js';

const RECOVERY_RETRY_INITIAL_DELAY_MS = 250;
const RECOVERY_RETRY_MAX_DELAY_MS = 5_000;

export type GoogleChatbotOptions = {
  config: GoogleChatbotConfig;
  gchatAdapter?: GoogleChatAdapter;
  logger?: Logger;
  state?: StateAdapter;
  stateStore?: GoogleChatThreadStateStore;
};

type RenderRecoveryScheduler = {
  schedule(delayMs?: number): void;
};

export type GoogleChatbotInstance = {
  app: Hono;
  chat: Chat<{ gchat: GoogleChatAdapter }, GoogleChatThreadState>;
  gchatAdapter: GoogleChatAdapter;
  googlechatbot: GoogleChatbotService;
  initialize(): Promise<void>;
  isReady(): boolean;
  logger: Logger;
  stateStore: GoogleChatThreadStateStore;
};

export function createGooglechatbot(options: GoogleChatbotOptions): GoogleChatbotInstance {
  const { config } = options;
  assertGoogleChatCredentials(config);
  assertGoogleChatWebhookVerification(config);
  assertGoogleChatPubsubVerification(config);

  const logger = options.logger ?? createGoogleChatbotLogger(config.server.logLevel);
  const stateStore = options.stateStore ?? createStateStore(config, logger.child('postgres-state'));
  const state = options.state ?? stateAdapterForStore(stateStore);
  const gchatAdapter = options.gchatAdapter ?? createGoogleChatAdapter({
    ...(config.gchat.useApplicationDefaultCredentials
      ? { useApplicationDefaultCredentials: true as const }
      : { credentials: config.gchat.credentials! }),
    ...(config.gchat.botUserId ? { botUserId: config.gchat.botUserId } : {}),
    ...(config.gchat.endpointUrl ? { endpointUrl: config.gchat.endpointUrl } : {}),
    ...(config.gchat.projectNumber ? { googleChatProjectNumber: config.gchat.projectNumber } : {}),
    ...(config.gchat.impersonateUser ? { impersonateUser: config.gchat.impersonateUser } : {}),
    ...(config.gchat.pubsubAudience ? { pubsubAudience: config.gchat.pubsubAudience } : {}),
    ...(config.gchat.pubsubServiceAccountEmail
      ? { pubsubServiceAccountEmail: config.gchat.pubsubServiceAccountEmail }
      : {}),
    ...(config.gchat.pubsubTopic ? { pubsubTopic: config.gchat.pubsubTopic } : {}),
    ...(config.gchat.workspaceAddOnServiceAccountEmail
      ? { workspaceAddOnServiceAccountEmail: config.gchat.workspaceAddOnServiceAccountEmail }
      : {}),
    logger,
    userName: config.gchat.userName,
  });

  const chat = new Chat<{ gchat: GoogleChatAdapter }, GoogleChatThreadState>({
    userName: config.gchat.userName,
    adapters: { gchat: gchatAdapter },
    state,
    concurrency: 'concurrent',
    fallbackStreamingPlaceholderText: null,
    logger,
  });

  let ready = false;
  let renderRecoveryScheduler: RenderRecoveryScheduler | undefined;

  const googlechatbot = new GoogleChatbotService(config, stateStore, undefined, {
    gchatAdapter,
    logger,
    onRenderObligationIndexed: () => renderRecoveryScheduler?.schedule(),
  });
  renderRecoveryScheduler = createRenderRecoveryScheduler(googlechatbot);

  chat.onDirectMessage(async (thread, message) => {
    await thread.subscribe();
    await googlechatbot.runChatMessage(thread, message, 'execute');
  });

  chat.onNewMention(async (thread, message) => {
    await thread.subscribe();
    await googlechatbot.runChatMessage(thread, message, 'execute');
  });

  chat.onSubscribedMessage(async (thread, message) => {
    await googlechatbot.runChatMessage(thread, message, message.isMention === true ? 'execute' : 'append');
  });

  const app = new Hono();

  app.get('/live', (c) => c.json({ ok: true, service: 'googlechatbot' }));

  app.get('/health', (c) => c.json({ ok: ready, service: 'googlechatbot' }, ready ? 200 : 503));
  app.get('/ready', (c) => c.json({ ok: ready, service: 'googlechatbot' }, ready ? 200 : 503));

  // Google Chat delivers both app events (direct webhook) and Workspace Events
  // Pub/Sub pushes to the same handler; the adapter tells them apart and
  // verifies each against its own audience.
  app.post('/api/webhooks/gchat', (c) => handleWebhook(c.req.raw));
  app.post('/api/webhooks/gchat/pubsub', (c) => handleWebhook(c.req.raw));

  async function handleWebhook(request: Request): Promise<Response> {
    return chat.webhooks.gchat(request, {
      waitUntil: (task) => {
        void task.catch((error) => {
          logger.error('googlechatbot_webhook_task_failed', {
            error: error instanceof Error ? error.message : String(error),
          });
        });
      },
    });
  }

  return {
    app,
    chat,
    gchatAdapter,
    googlechatbot,
    async initialize() {
      await ensureStateConnected(state, logger);
      await chat.initialize();
      ready = true;
      renderRecoveryScheduler?.schedule();
    },
    isReady: () => ready,
    logger,
    stateStore,
  };
}

function createStateStore(config: GoogleChatbotConfig, logger: Logger): PostgresGoogleChatbotStateStore {
  if (!config.server.postgresUrl) {
    throw new Error('GOOGLECHATBOT_DATABASE_URL (or DATABASE_URL / POSTGRES_URL) is required');
  }
  return new PostgresGoogleChatbotStateStore({
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
      logger.warn('googlechatbot_postgres_connect_retry', {
        attempt: attempt + 1,
        delayMs,
        error: error instanceof Error ? error.message : String(error),
      });
      await sleep(delayMs);
    }
  }
}

function stateAdapterForStore(stateStore: GoogleChatThreadStateStore): StateAdapter {
  if (stateStore instanceof PostgresGoogleChatbotStateStore) {
    return stateStore.adapter;
  }
  throw new Error('A Chat SDK StateAdapter is required when using a custom Googlechatbot state store');
}

export function createRenderRecoveryScheduler(
  googlechatbot: Pick<GoogleChatbotService, 'logger' | 'recoverRenderObligations'>,
): RenderRecoveryScheduler {
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
      const deferredCount = await googlechatbot.recoverRenderObligations();
      if (deferredCount === 0) {
        attempt = 0;
        return;
      }
      retryDelayMs = Math.min(RECOVERY_RETRY_INITIAL_DELAY_MS * 2 ** attempt, RECOVERY_RETRY_MAX_DELAY_MS);
      attempt += 1;
      googlechatbot.logger.warn('googlechatbot_render_recovery_retry_scheduled', {
        deferredCount,
        delayMs: retryDelayMs,
        attempt,
      });
    } catch (error) {
      googlechatbot.logger.error('googlechatbot_render_recovery_loop_failed', { error });
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

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
