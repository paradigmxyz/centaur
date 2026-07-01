import { describe, expect, it } from 'bun:test';
import { InMemoryTeamsThreadStateStore } from './support/in-memory-state.js';

describe('Teams render recovery state stores', () => {
  it('indexes stranded render obligations for recovery scans', async () => {
    const store = new InMemoryTeamsThreadStateStore();

    await store.indexRenderObligation('thread-1', { maxLength: 2, ttlMs: 60_000 });
    await store.indexRenderObligation('thread-2', { maxLength: 2, ttlMs: 60_000 });
    await store.indexRenderObligation('thread-3', { maxLength: 2, ttlMs: 60_000 });

    await expect(store.listRenderObligationThreadKeys()).resolves.toEqual(['thread-2', 'thread-3']);
  });

  it('uses a per-thread recovery lease', async () => {
    const store = new InMemoryTeamsThreadStateStore();

    const release = await store.acquireRenderRecoveryLease('thread-1', 60_000);

    expect(release).toBeFunction();
    await expect(store.acquireRenderRecoveryLease('thread-1', 60_000)).resolves.toBeNull();
    await release?.();
    await expect(store.acquireRenderRecoveryLease('thread-1', 60_000)).resolves.toBeFunction();
  });

  it('lets live render leases block recovery leases until released', async () => {
    const store = new InMemoryTeamsThreadStateStore();

    const release = await store.acquireLiveRenderLease('thread-1', 60_000);

    await expect(store.acquireRenderRecoveryLease('thread-1', 60_000)).resolves.toBeNull();
    await release();
    await expect(store.acquireRenderRecoveryLease('thread-1', 60_000)).resolves.toBeFunction();
  });

  it('uses a per-message inbound lease', async () => {
    const store = new InMemoryTeamsThreadStateStore();

    const release = await store.acquireInboundMessageLease('thread-1', 'message-1', 60_000);

    expect(release).toBeFunction();
    await expect(store.acquireInboundMessageLease('thread-1', 'message-1', 60_000)).resolves.toBeNull();
    await release?.();
    await expect(store.acquireInboundMessageLease('thread-1', 'message-1', 60_000)).resolves.toBeFunction();
  });

  it('uses a per-thread turn lease', async () => {
    const store = new InMemoryTeamsThreadStateStore();

    const release = await store.acquireThreadTurnLease('thread-1', 60_000);

    expect(release).toBeFunction();
    await expect(store.acquireThreadTurnLease('thread-1', 60_000)).resolves.toBeNull();
    await release?.();
    await expect(store.acquireThreadTurnLease('thread-1', 60_000)).resolves.toBeFunction();
  });
});
