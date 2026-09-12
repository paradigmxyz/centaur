import { randomUUID } from 'node:crypto';
import { createPostgresState } from '@chat-adapter/state-pg';
import type { Logger, StateAdapter } from 'chat';
import pg from 'pg';
import type {
  GoogleChatRenderRecoveryStateStore,
  GoogleChatThreadState,
  GoogleChatThreadStateStore,
  SpaceReferenceStore,
  StoredSpaceReference,
} from './types.js';

const RENDER_OBLIGATION_INDEX_KEY = 'googlechatbot:render:index';
const THREAD_INDEX_KEY = 'googlechatbot:threads:index';
const MIN_RENDER_LEASE_REFRESH_INTERVAL_MS = 1000;

export type PostgresGoogleChatbotStateStoreOptions = {
  logger?: Logger;
  postgresUrl: string;
  stateKeyPrefix?: string;
};

export class PostgresGoogleChatbotStateStore implements GoogleChatRenderRecoveryStateStore, SpaceReferenceStore {
  readonly adapter: StateAdapter;
  private readonly pool: pg.Pool;

  constructor(options: PostgresGoogleChatbotStateStoreOptions) {
    this.pool = new pg.Pool({ connectionString: options.postgresUrl });
    this.pool.on('error', (error) => {
      options.logger?.warn('postgres pool error', { error: errorMessage(error) });
    });
    this.adapter = createPostgresState({
      client: this.pool,
      keyPrefix: options.stateKeyPrefix ?? 'centaur-googlechatbot',
      logger: options.logger,
    });
  }

  async connect(): Promise<void> {
    await this.adapter.connect();
  }

  async disconnect(): Promise<void> {
    await this.adapter.disconnect();
  }

  async get(threadKey: string): Promise<GoogleChatThreadState | undefined> {
    return (await this.adapter.get<GoogleChatThreadState>(threadStateKey(threadKey))) ?? undefined;
  }

  async list(): Promise<Array<{ state: GoogleChatThreadState; threadKey: string }>> {
    const threadKeys = Array.from(new Set(await this.adapter.getList<string>(THREAD_INDEX_KEY)));
    const entries = await Promise.all(threadKeys.map(async (threadKey) => {
      const state = await this.get(threadKey);
      return state ? { state, threadKey } : undefined;
    }));
    return entries.filter((entry): entry is { state: GoogleChatThreadState; threadKey: string } => Boolean(entry));
  }

  async set(threadKey: string, state: GoogleChatThreadState): Promise<void> {
    await this.adapter.set(threadStateKey(threadKey), structuredClone(state));
    await this.adapter.appendToList(THREAD_INDEX_KEY, threadKey, { maxLength: 10_000 });
  }

  async getReference(threadKey: string): Promise<StoredSpaceReference | undefined> {
    return (await this.adapter.get<StoredSpaceReference>(referenceKey(threadKey))) ?? undefined;
  }

  async setReference(threadKey: string, reference: StoredSpaceReference): Promise<void> {
    await this.adapter.set(referenceKey(threadKey), structuredClone(reference));
  }

  async indexRenderObligation(threadKey: string, options: { maxLength: number; ttlMs: number }): Promise<void> {
    await this.adapter.appendToList(RENDER_OBLIGATION_INDEX_KEY, threadKey, options);
  }

  async listRenderObligationThreadKeys(): Promise<string[]> {
    return Array.from(new Set(await this.adapter.getList<string>(RENDER_OBLIGATION_INDEX_KEY)));
  }

  async acquireLiveRenderLease(threadKey: string, ttlMs: number): Promise<() => Promise<void>> {
    const release = await this.acquireRenderLease(threadKey, ttlMs, { onlyIfMissing: false });
    if (!release) {
      throw new Error('live render lease acquisition failed');
    }
    return release;
  }

  async acquireRenderRecoveryLease(threadKey: string, ttlMs: number): Promise<(() => Promise<void>) | null> {
    return this.acquireRenderLease(threadKey, ttlMs, { onlyIfMissing: true });
  }

  async acquireInboundMessageLease(threadKey: string, messageId: string, ttlMs: number): Promise<(() => Promise<void>) | null> {
    return this.acquireStateLease(inboundMessageLeaseKey(threadKey, messageId), ttlMs);
  }

  async acquireThreadTurnLease(threadKey: string, ttlMs: number): Promise<(() => Promise<void>) | null> {
    return this.acquireStateLease(threadTurnLeaseKey(threadKey), ttlMs);
  }

  private async acquireStateLease(key: string, ttlMs: number): Promise<(() => Promise<void>) | null> {
    const token = randomUUID();
    await this.adapter.get<string>(key);
    if (!(await this.adapter.setIfNotExists(key, token, ttlMs))) {
      return null;
    }
    return async () => {
      try {
        const activeToken = await this.adapter.get<string>(key);
        if (activeToken === token) {
          await this.adapter.delete(key);
        }
      } catch {
        // TTL expiry is the backstop.
      }
    };
  }

  private async acquireRenderLease(
    threadKey: string,
    ttlMs: number,
    options: { onlyIfMissing: boolean },
  ): Promise<(() => Promise<void>) | null> {
    const key = renderRecoveryLeaseKey(threadKey);
    const token = randomUUID();
    if (options.onlyIfMissing) {
      // state-pg removes expired cache rows on get(), while setIfNotExists()
      // still conflicts with them. Touch first so an expired live lease is
      // claimable by recovery after its TTL.
      await this.adapter.get<string>(key);
      if (!(await this.adapter.setIfNotExists(key, token, ttlMs))) {
        return null;
      }
    }
    if (!options.onlyIfMissing) {
      await this.adapter.set(key, token, ttlMs);
    }
    const refresh = setInterval(() => {
      void this.adapter
        .get<string>(key)
        .then((activeToken) => activeToken === token ? this.adapter.set(key, token, ttlMs) : undefined)
        .catch(() => undefined);
    }, Math.max(MIN_RENDER_LEASE_REFRESH_INTERVAL_MS, Math.floor(ttlMs / 2)));
    return async () => {
      clearInterval(refresh);
      try {
        const activeToken = await this.adapter.get<string>(key);
        if (activeToken === token) {
          await this.adapter.delete(key);
        }
      } catch {
        // TTL expiry is the backstop.
      }
    };
  }
}

function threadStateKey(threadKey: string): string {
  return `thread-state:${threadKey}`;
}

function referenceKey(threadKey: string): string {
  return `googlechatbot:reference:${threadKey}`;
}

function renderRecoveryLeaseKey(threadKey: string): string {
  return `googlechatbot:render:lease:${threadKey}`;
}

function inboundMessageLeaseKey(threadKey: string, messageId: string): string {
  return `googlechatbot:inbound:lease:${threadKey}:${messageId}`;
}

function threadTurnLeaseKey(threadKey: string): string {
  return `googlechatbot:turn:lease:${threadKey}`;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
