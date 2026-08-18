import { randomUUID } from 'node:crypto';
import type {
  ConversationReferenceStore,
  StoredConversationReference,
  TeamsRenderRecoveryStateStore,
  TeamsThreadState,
} from '../../src/types.js';

export class InMemoryTeamsThreadStateStore implements TeamsRenderRecoveryStateStore, ConversationReferenceStore {
  private readonly states = new Map<string, TeamsThreadState>();
  private readonly references = new Map<string, StoredConversationReference>();
  private readonly renderObligationThreadKeys: string[] = [];
  private readonly inboundMessageLeases = new Map<string, string>();
  private readonly renderRecoveryLeases = new Map<string, string>();
  private readonly threadTurnLeases = new Map<string, string>();

  async get(threadKey: string): Promise<TeamsThreadState | undefined> {
    const state = this.states.get(threadKey);
    return state ? structuredClone(state) : undefined;
  }

  async list(): Promise<Array<{ state: TeamsThreadState; threadKey: string }>> {
    return [...this.states.entries()].map(([threadKey, state]) => ({
      threadKey,
      state: structuredClone(state),
    }));
  }

  async set(threadKey: string, state: TeamsThreadState): Promise<void> {
    this.states.set(threadKey, structuredClone(state));
  }

  async indexRenderObligation(threadKey: string, options: { maxLength: number; ttlMs: number }): Promise<void> {
    this.renderObligationThreadKeys.push(threadKey);
    if (this.renderObligationThreadKeys.length > options.maxLength) {
      this.renderObligationThreadKeys.splice(0, this.renderObligationThreadKeys.length - options.maxLength);
    }
  }

  async listRenderObligationThreadKeys(): Promise<string[]> {
    return [...this.renderObligationThreadKeys];
  }

  async acquireInboundMessageLease(threadKey: string, messageId: string, _ttlMs: number): Promise<(() => Promise<void>) | null> {
    return acquireMapLease(this.inboundMessageLeases, inboundMessageLeaseKey(threadKey, messageId));
  }

  async acquireThreadTurnLease(threadKey: string, _ttlMs: number): Promise<(() => Promise<void>) | null> {
    return acquireMapLease(this.threadTurnLeases, threadTurnLeaseKey(threadKey));
  }

  async acquireLiveRenderLease(threadKey: string, _ttlMs: number): Promise<() => Promise<void>> {
    const token = randomUUID();
    this.renderRecoveryLeases.set(threadKey, token);
    return async () => {
      if (this.renderRecoveryLeases.get(threadKey) === token) {
        this.renderRecoveryLeases.delete(threadKey);
      }
    };
  }

  async acquireRenderRecoveryLease(threadKey: string, _ttlMs: number): Promise<(() => Promise<void>) | null> {
    if (this.renderRecoveryLeases.has(threadKey)) {
      return null;
    }
    const token = randomUUID();
    this.renderRecoveryLeases.set(threadKey, token);
    return async () => {
      if (this.renderRecoveryLeases.get(threadKey) === token) {
        this.renderRecoveryLeases.delete(threadKey);
      }
    };
  }

  async getReference(threadKey: string): Promise<StoredConversationReference | undefined> {
    return this.getConversationReference(threadKey);
  }

  async getConversationReference(threadKey: string): Promise<StoredConversationReference | undefined> {
    const reference = this.references.get(threadKey);
    return reference ? structuredClone(reference) : undefined;
  }

  async setReference(threadKey: string, reference: StoredConversationReference): Promise<void> {
    return this.setConversationReference(threadKey, reference);
  }

  async setConversationReference(threadKey: string, reference: StoredConversationReference): Promise<void> {
    this.references.set(threadKey, structuredClone(reference));
  }
}

function inboundMessageLeaseKey(threadKey: string, messageId: string): string {
  return `teamsbot:inbound:lease:${threadKey}:${messageId}`;
}

function threadTurnLeaseKey(threadKey: string): string {
  return `teamsbot:turn:lease:${threadKey}`;
}

function acquireMapLease(leases: Map<string, string>, key: string): (() => Promise<void>) | null {
  if (leases.has(key)) {
    return null;
  }
  const token = randomUUID();
  leases.set(key, token);
  return async () => {
    if (leases.get(key) === token) {
      leases.delete(key);
    }
  };
}
