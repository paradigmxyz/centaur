import { harnessToChatSdkStream, type ChatSDKStreamChunk } from '@centaur/rendering';
import type { GoogleChatAdapter } from '@chat-adapter/gchat';
import type { Logger, Message as ChatMessage, Thread } from 'chat';
import type { GoogleChatbotConfig } from './config.js';
import { conflateGoogleChatRenderStream, type GoogleChatRenderChunk } from './conflate.js';
import { hydrateGoogleChatAttachments } from './googlechat-attachments.js';
import {
  decodeGoogleChatThreadKey,
  isAllowedGoogleChatMessage,
  isDirectMessageSpace,
  serializeGoogleChatMessage,
} from './googlechat-message.js';
import { createGoogleChatbotLogger } from './logger.js';
import {
  createAdapterReplySink,
  createChatReplySink,
  type GoogleChatReplySink,
  type GoogleChatReplySinkResult,
} from './reply-sink.js';
import { CentaurSessionClient, SessionApiError } from './session-api.js';
import type {
  GoogleChatApiMessage,
  GoogleChatEventEnvelope,
  GoogleChatRenderRecoveryStateStore,
  GoogleChatThreadState,
  GoogleChatThreadStateStore,
  SpaceReferenceStore,
  StoredSpaceReference,
} from './types.js';

const RENDER_INDEX_TTL_MS = 7 * 24 * 60 * 60 * 1000;
const RENDER_OBLIGATION_INDEX_MAX_LENGTH = 2000;
const RENDER_RECOVERY_LEASE_TTL_MS = 2 * 60 * 1000;
const RENDER_RECOVERY_LEASE_TIMEOUT_SAFETY_MS = 1_000;
const INBOUND_MESSAGE_LEASE_TTL_MS = 30 * 60 * 1000;
const THREAD_TURN_LEASE_TTL_MS = 30_000;

type GoogleChatbotServiceOptions = {
  gchatAdapter?: GoogleChatAdapter;
  logger?: Logger;
  onRenderObligationIndexed?: () => void;
  recoverySinkFactory?: (
    threadKey: string,
    reference: StoredSpaceReference,
    messageId?: string,
  ) => GoogleChatReplySink | undefined;
  spaceReferenceStore?: SpaceReferenceStore;
};

export class GoogleChatbotService {
  private readonly sessionClient: CentaurSessionClient;
  private readonly gchatAdapter?: GoogleChatAdapter;
  readonly logger: Logger;
  private readonly onRenderObligationIndexed?: () => void;
  private readonly recoverySinkFactory?: GoogleChatbotServiceOptions['recoverySinkFactory'];
  private readonly recoveryStateStore?: GoogleChatRenderRecoveryStateStore;
  private readonly spaceReferenceStore?: SpaceReferenceStore;
  private readonly threadLocks = new Map<string, Promise<void>>();

  constructor(
    private readonly config: GoogleChatbotConfig,
    private readonly stateStore: GoogleChatThreadStateStore,
    sessionClient?: CentaurSessionClient,
    options: GoogleChatbotServiceOptions = {},
  ) {
    this.logger = options.logger ?? createGoogleChatbotLogger(config.server.logLevel);
    this.sessionClient = sessionClient ?? new CentaurSessionClient({
      apiKey: config.centaur.apiKey,
      apiUrl: config.centaur.apiUrl,
      defaultHarnessType: config.gchat.defaultHarnessType,
      idleTimeoutMs: config.gchat.idleTimeoutMs,
      logger: this.logger.child('session-api'),
      maxDurationMs: config.gchat.maxDurationMs,
      requestMaxRetries: config.centaur.requestMaxRetries,
      requestRetryDelayMs: config.centaur.requestRetryDelayMs,
    });
    this.spaceReferenceStore = options.spaceReferenceStore
      ?? (isSpaceReferenceStore(stateStore) ? stateStore : undefined);
    this.recoveryStateStore = isRenderRecoveryStateStore(stateStore) ? stateStore : undefined;
    this.onRenderObligationIndexed = options.onRenderObligationIndexed;
    this.recoverySinkFactory = options.recoverySinkFactory;
    this.gchatAdapter = options.gchatAdapter;
  }

  async runChatMessage(
    thread: Thread<GoogleChatThreadState>,
    chatMessage: ChatMessage,
    mode: 'append' | 'execute',
  ): Promise<void> {
    const envelope = chatMessage.raw as GoogleChatEventEnvelope | undefined;
    if (!envelope || typeof envelope !== 'object') {
      return;
    }
    await this.handleChatMessage(thread, chatMessage, envelope, mode);
  }

  private async handleChatMessage(
    thread: Thread<GoogleChatThreadState>,
    chatMessage: ChatMessage,
    envelope: GoogleChatEventEnvelope,
    mode: 'append' | 'execute',
  ): Promise<void> {
    const threadKey = thread.id;
    if (!decodeGoogleChatThreadKey(threadKey)) {
      return;
    }
    const mentioned = chatMessage.isMention === true;
    const serialized = serializeGoogleChatMessage(envelope, threadKey, chatMessage.text);
    serialized.isMention = mentioned;
    if (serialized.author.isBot) {
      return;
    }
    if (!isAllowedGoogleChatMessage({
      allowDirectMessages: this.config.gchat.allowDirectMessages,
      allowedDomains: this.config.gchat.allowedDomains,
      allowedSenderEmails: this.config.gchat.allowedSenderEmails,
      allowedSpaceIds: this.config.gchat.allowedSpaceIds,
      message: serialized,
      threadKey,
    })) {
      return;
    }

    await this.spaceReferenceStore?.setReference(threadKey, toStoredSpaceReference(serialized, threadKey));
    const existing = await this.getThreadState(threadKey, thread);
    const active = existing?.active === true
      || mentioned
      || mode === 'execute'
      || !this.config.gchat.requireMention;
    if (!active) {
      return;
    }

    const message: GoogleChatApiMessage = {
      ...serialized,
      attachments: await hydrateGoogleChatAttachments(serialized.attachments, chatMessage.attachments, {
        enabled: this.config.gchat.attachmentDownloadEnabled,
        maxBytes: this.config.gchat.attachmentMaxBytes,
      }),
    };
    if (!message.text.trim() && message.attachments.length === 0) {
      await thread.post('Send a message or attach a file and I will pass it to Centaur.');
      return;
    }

    const action = await this.claimTurnAction(thread, threadKey, existing, message.id, {
      forceAppend: mode === 'append',
    });
    if (action.kind === 'noop') {
      return;
    }
    if (action.kind === 'append') {
      try {
        await this.withThreadLock(threadKey, async () => {
          await this.sessionClient.createSession(threadKey, message);
          await this.sessionClient.appendMessages(threadKey, [message]);
          const latest = await this.getThreadState(threadKey, thread);
          await this.setThreadState(threadKey, {
            ...latest,
            active: true,
            forwardedMessageIds: appendUnique(
              latest?.forwardedMessageIds ?? action.existing?.forwardedMessageIds,
              message.id,
            ),
          }, thread);
        });
      } finally {
        await this.finishAppendClaim(thread, threadKey).catch(() => undefined);
        await action.releaseThreadTurnLease?.().catch(() => undefined);
        await action.releaseInboundMessageLease?.().catch(() => undefined);
      }
      return;
    }

    try {
      await this.executeMessage(
        thread,
        threadKey,
        message,
        action.existing,
        createChatReplySink(thread, { minEditIntervalMs: this.config.gchat.renderMinEditIntervalMs }),
      );
    } finally {
      await action.releaseInboundMessageLease?.().catch(() => undefined);
    }
  }

  private async claimTurnAction(
    thread: Thread<GoogleChatThreadState>,
    threadKey: string,
    existing: GoogleChatThreadState | undefined,
    messageId: string,
    options: { forceAppend?: boolean } = {},
  ): Promise<{
    existing: GoogleChatThreadState | undefined;
    kind: 'append' | 'execute' | 'noop';
    releaseInboundMessageLease?: () => Promise<void>;
    releaseThreadTurnLease?: () => Promise<void>;
  }> {
    const releaseInboundMessageLease = this.recoveryStateStore
      ? await this.recoveryStateStore.acquireInboundMessageLease(threadKey, messageId, INBOUND_MESSAGE_LEASE_TTL_MS)
      : undefined;
    if (this.recoveryStateStore && !releaseInboundMessageLease) {
      return { existing, kind: 'noop' };
    }
    const releaseInboundLease = releaseInboundMessageLease ?? undefined;
    let releaseThreadTurnLease: (() => Promise<void>) | undefined;
    try {
      releaseThreadTurnLease = await this.acquireThreadTurnLease(threadKey);
      const action = await this.withThreadLock(threadKey, async () => {
        const latest = await this.getThreadState(threadKey, thread) ?? existing;
        if (latest?.forwardedMessageIds?.includes(messageId) || latest?.executedMessageIds?.includes(messageId)) {
          await releaseThreadTurnLease?.().catch(() => undefined);
          releaseThreadTurnLease = undefined;
          await releaseInboundLease?.().catch(() => undefined);
          return { existing: latest, kind: 'noop' as const };
        }
        return this.claimTurnActionUnderLease(
          thread,
          threadKey,
          latest,
          existing,
          releaseInboundLease,
          releaseThreadTurnLease,
          options,
        );
      });
      if (action.kind !== 'append') {
        await releaseThreadTurnLease?.().catch(() => undefined);
        action.releaseThreadTurnLease = undefined;
      }
      return action;
    } catch (error) {
      await releaseThreadTurnLease?.().catch(() => undefined);
      await releaseInboundLease?.().catch(() => undefined);
      throw error;
    }
  }

  private async claimTurnActionUnderLease(
    thread: Thread<GoogleChatThreadState>,
    threadKey: string,
    latest: GoogleChatThreadState | undefined,
    observed: GoogleChatThreadState | undefined,
    releaseInboundMessageLease: (() => Promise<void>) | undefined,
    releaseThreadTurnLease: (() => Promise<void>) | undefined,
    options: { forceAppend?: boolean } = {},
  ): Promise<{
    existing: GoogleChatThreadState | undefined;
    kind: 'append' | 'execute' | 'noop';
    releaseInboundMessageLease?: () => Promise<void>;
    releaseThreadTurnLease?: () => Promise<void>;
  }> {
    if (options.forceAppend
      || shouldAppendTurn(latest, this.config.gchat.activeExecutionTtlMs)
      || shouldAppendTurn(observed, this.config.gchat.activeExecutionTtlMs)) {
      await this.setThreadState(threadKey, {
        ...(latest ?? { active: true }),
        active: true,
        appendInFlight: (latest?.appendInFlight ?? 0) + 1,
      }, thread);
      return { existing: latest, kind: 'append', releaseInboundMessageLease, releaseThreadTurnLease };
    }
    await this.setThreadState(threadKey, {
      ...(latest ?? { active: true }),
      active: true,
      activeExecution: true,
      activeExecutionStartedAt: Date.now(),
    }, thread);
    return { existing: latest, kind: 'execute', releaseInboundMessageLease };
  }

  private async acquireThreadTurnLease(threadKey: string): Promise<(() => Promise<void>) | undefined> {
    if (!this.recoveryStateStore) {
      return undefined;
    }
    const deadline = Date.now() + THREAD_TURN_LEASE_TTL_MS;
    for (;;) {
      const release = await this.recoveryStateStore.acquireThreadTurnLease(threadKey, THREAD_TURN_LEASE_TTL_MS);
      if (release) {
        return release;
      }
      if (Date.now() >= deadline) {
        throw new Error('Google Chat thread turn lease is unavailable');
      }
      await sleep(25);
    }
  }

  private async withThreadLock<T>(threadKey: string, action: () => Promise<T>): Promise<T> {
    const previous = this.threadLocks.get(threadKey) ?? Promise.resolve();
    let release!: () => void;
    const current = new Promise<void>((resolve) => {
      release = resolve;
    });
    const tail = previous.catch(() => undefined).then(() => current);
    this.threadLocks.set(threadKey, tail);
    await previous.catch(() => undefined);
    try {
      return await action();
    } finally {
      release();
      if (this.threadLocks.get(threadKey) === tail) {
        this.threadLocks.delete(threadKey);
      }
    }
  }

  private async finishAppendClaim(thread: Thread<GoogleChatThreadState>, threadKey: string): Promise<void> {
    await this.withThreadLock(threadKey, async () => {
      const latest = await this.getThreadState(threadKey, thread);
      const appendInFlight = Math.max((latest?.appendInFlight ?? 0) - 1, 0);
      await this.setThreadState(threadKey, {
        ...(latest ?? { active: true }),
        active: true,
        appendInFlight,
        ...(appendInFlight > 0
          ? {
            activeExecution: true,
            activeExecutionStartedAt: latest?.activeExecutionStartedAt ?? Date.now(),
            appendBarrier: latest?.appendBarrier,
          }
          : latest?.appendBarrier
            ? { activeExecution: false, activeExecutionStartedAt: null, appendBarrier: false }
            : { appendBarrier: false }),
      }, thread);
    });
  }

  private async executeMessage(
    thread: Thread<GoogleChatThreadState>,
    threadKey: string,
    message: GoogleChatApiMessage,
    existing: GoogleChatThreadState | undefined,
    sink: GoogleChatReplySink,
  ): Promise<void> {
    let progressMessageId: string | undefined;
    let lastEventId = existing?.lastEventId ?? 0;
    let releaseRenderLease: (() => Promise<void>) | null = null;

    await this.withDistributedThreadTurnLock(threadKey, async () => {
      await this.setThreadState(threadKey, {
        ...((await this.getThreadState(threadKey, thread)) ?? existing ?? { active: true }),
        active: true,
        activeExecution: true,
        activeExecutionStartedAt: Date.now(),
      }, thread);
    });

    try {
      ({ progressMessageId } = await withTimeout(
        sink.begin(),
        this.config.gchat.activeExecutionTtlMs,
        'begin Google Chat live render',
      ));
      await this.sessionClient.createSession(threadKey, message);
      const alreadyForwarded = new Set(existing?.forwardedMessageIds ?? []);
      if (!alreadyForwarded.has(message.id)) {
        await this.sessionClient.appendMessages(threadKey, [message]);
      }
      releaseRenderLease = await this.recoveryStateStore?.acquireLiveRenderLease(threadKey, RENDER_RECOVERY_LEASE_TTL_MS) ?? null;
      if (!releaseRenderLease) {
        throw new Error('Google Chat render lease is unavailable');
      }
      await this.withDistributedThreadTurnLock(threadKey, async () => {
        const latest = await this.getThreadState(threadKey, thread);
        await this.setThreadState(threadKey, {
          ...(latest ?? existing ?? { active: true }),
          active: true,
          activeExecution: true,
          activeExecutionStartedAt: Date.now(),
          executedMessageIds: appendUnique(latest?.executedMessageIds ?? existing?.executedMessageIds, message.id),
          forwardedMessageIds: appendUnique(latest?.forwardedMessageIds ?? existing?.forwardedMessageIds, message.id),
          lastEventId,
          renderObligation: {
            afterEventId: lastEventId,
            message,
            progressMessageId,
          },
        }, thread);
      });
      await this.indexRenderObligation(threadKey);
      const execution = await withAbortTimeout(
        (signal) => this.sessionClient.executeSession(threadKey, message, { signal }),
        this.config.gchat.activeExecutionTtlMs,
        'execute Google Chat live session',
      );
      await this.withDistributedThreadTurnLock(threadKey, async () => {
        const latestAfterExecute = await this.getThreadState(threadKey, thread);
        await this.setThreadState(threadKey, {
          ...latestAfterExecute,
          active: true,
          activeExecution: true,
          activeExecutionStartedAt: Date.now(),
          executedMessageIds: appendUnique(latestAfterExecute?.executedMessageIds, message.id),
          lastEventId,
          renderObligation: {
            afterEventId: lastEventId,
            executionId: execution.execution_id,
            message,
            progressMessageId,
          },
        }, thread);
      });

      lastEventId = await this.renderExecutionStream({
        afterEventId: lastEventId,
        deliveryTimeoutMs: this.config.gchat.renderDeliveryTimeoutMs,
        executionId: execution.execution_id,
        sink,
        stateThread: thread,
        threadKey,
        timeoutMs: this.config.gchat.activeExecutionTtlMs,
      });
      await this.withDistributedThreadTurnLock(threadKey, async () => {
        await this.setThreadState(threadKey, {
          ...completionState(await this.getThreadState(threadKey, thread)),
          active: true,
          lastEventId,
          renderObligation: null,
        }, thread);
      });
      await releaseRenderLease?.();
      releaseRenderLease = null;
    } catch (error) {
      const latest = await this.getThreadState(threadKey, thread);
      if (isRetryableRecoveryError(error) && latest?.renderObligation) {
        await this.withDistributedThreadTurnLock(threadKey, async () => {
          await this.setThreadState(threadKey, {
            ...(await this.getThreadState(threadKey, thread)),
            active: true,
            activeExecution: false,
            activeExecutionStartedAt: null,
            lastEventId,
          }, thread);
        });
        await this.indexRenderObligation(threadKey);
      } else {
        const messageText = error instanceof Error ? error.message : String(error);
        await failBestEffort(sink, `Error: ${messageText}`, this.logger, this.config.gchat.activeExecutionTtlMs);
        await this.withDistributedThreadTurnLock(threadKey, async () => {
          await this.setThreadState(threadKey, {
            ...(await this.getThreadState(threadKey, thread)),
            active: true,
            activeExecution: false,
            activeExecutionStartedAt: null,
            lastEventId,
            renderObligation: null,
          }, thread);
        });
      }
    } finally {
      await releaseRenderLease?.().catch(() => undefined);
    }
  }

  async recoverRenderObligations(): Promise<number> {
    let deferredCount = 0;
    for (const threadKey of await this.renderObligationThreadKeys()) {
      const releaseLease = await this.recoveryStateStore?.acquireRenderRecoveryLease(threadKey, RENDER_RECOVERY_LEASE_TTL_MS);
      if (!releaseLease) {
        deferredCount += 1;
        this.logger.warn('gchat_render_recovery_lease_skipped', { threadKey });
        continue;
      }
      const state = await this.getThreadState(threadKey);
      if (!state?.renderObligation) {
        await releaseLease();
        continue;
      }
      const obligation = state.renderObligation;
      const storedReference = await this.spaceReferenceStore?.getReference(threadKey);
      if (!storedReference) {
        this.logger.warn('gchat_render_recovery_skipped_no_reference', { threadKey });
        await this.clearRenderObligation(threadKey);
        await releaseLease();
        continue;
      }
      const sink = this.createRecoverySink(threadKey, storedReference, obligation.progressMessageId);
      if (!sink) {
        this.logger.warn('gchat_render_recovery_skipped_no_adapter', { threadKey });
        await this.clearRenderObligation(threadKey);
        await releaseLease();
        continue;
      }
      const timeoutMs = this.recoveryAttemptTimeoutMs();
      try {
        await withTimeout(sink.begin(), timeoutMs, 'begin Google Chat recovery render');
        const executionId = await this.ensureRecoveryExecutionId(threadKey, obligation, timeoutMs);
        await this.persistRecoveryExecutionId(threadKey, obligation, executionId);
        const lastEventId = await this.renderExecutionStream({
          afterEventId: obligation.afterEventId,
          deliveryTimeoutMs: this.config.gchat.renderDeliveryTimeoutMs,
          executionId,
          sink,
          threadKey,
          timeoutMs,
        });
        await this.withDistributedThreadTurnLock(threadKey, async () => {
          await this.setThreadState(threadKey, {
            ...(await this.getThreadState(threadKey)),
            active: true,
            activeExecution: false,
            activeExecutionStartedAt: null,
            lastEventId,
            renderObligation: null,
          });
        });
      } catch (error) {
        if (isRetryableRecoveryError(error)) {
          deferredCount += 1;
        } else {
          const messageText = error instanceof Error ? error.message : String(error);
          await failBestEffort(sink, `Error: ${messageText}`, this.logger, timeoutMs);
          await this.withDistributedThreadTurnLock(threadKey, async () => {
            await this.setThreadState(threadKey, {
              ...(await this.getThreadState(threadKey)),
              active: true,
              activeExecution: false,
              activeExecutionStartedAt: null,
              renderObligation: null,
            });
          });
        }
        this.logger.error('gchat_render_recovery_failed', { error, threadKey });
      } finally {
        await releaseLease().catch(() => undefined);
      }
    }
    return deferredCount;
  }

  private async ensureRecoveryExecutionId(
    threadKey: string,
    obligation: NonNullable<GoogleChatThreadState['renderObligation']>,
    timeoutMs: number,
  ): Promise<string> {
    if (obligation.executionId) {
      return obligation.executionId;
    }
    const execution = await withAbortTimeout(
      (signal) => this.sessionClient.executeSession(threadKey, obligation.message, { signal }),
      timeoutMs,
      'execute Google Chat recovery session',
    );
    return execution.execution_id;
  }

  private async persistRecoveryExecutionId(
    threadKey: string,
    obligation: NonNullable<GoogleChatThreadState['renderObligation']>,
    executionId: string,
  ): Promise<void> {
    if (obligation.executionId) {
      return;
    }
    await this.withDistributedThreadTurnLock(threadKey, async () => {
      const latest = await this.getThreadState(threadKey);
      if (!samePendingRenderObligation(latest?.renderObligation, obligation)) {
        return;
      }
      await this.setThreadState(threadKey, {
        ...(latest ?? { active: true }),
        active: true,
        activeExecution: true,
        activeExecutionStartedAt: Date.now(),
        executedMessageIds: appendUnique(latest?.executedMessageIds, obligation.message.id),
        renderObligation: {
          ...obligation,
          executionId,
        },
      });
    });
  }

  private async clearRenderObligation(threadKey: string): Promise<void> {
    await this.withDistributedThreadTurnLock(threadKey, async () => {
      await this.setThreadState(threadKey, {
        ...(await this.getThreadState(threadKey)),
        active: true,
        activeExecution: false,
        activeExecutionStartedAt: null,
        renderObligation: null,
      });
    });
  }

  private async withDistributedThreadTurnLock<T>(threadKey: string, action: () => Promise<T>): Promise<T> {
    const releaseThreadTurnLease = await this.acquireThreadTurnLease(threadKey);
    try {
      return await this.withThreadLock(threadKey, action);
    } finally {
      await releaseThreadTurnLease?.().catch(() => undefined);
    }
  }

  private recoveryAttemptTimeoutMs(): number {
    return Math.max(
      1,
      Math.min(
        this.config.gchat.activeExecutionTtlMs,
        RENDER_RECOVERY_LEASE_TTL_MS - RENDER_RECOVERY_LEASE_TIMEOUT_SAFETY_MS,
      ),
    );
  }

  private async renderObligationThreadKeys(): Promise<string[]> {
    if (this.recoveryStateStore) {
      return this.recoveryStateStore.listRenderObligationThreadKeys();
    }
    const entries = await this.stateStore.list();
    return entries
      .filter(({ state }) => Boolean(state.renderObligation))
      .map(({ threadKey }) => threadKey);
  }

  private async indexRenderObligation(threadKey: string): Promise<void> {
    if (!this.recoveryStateStore) {
      return;
    }
    await this.recoveryStateStore.indexRenderObligation(threadKey, {
      maxLength: RENDER_OBLIGATION_INDEX_MAX_LENGTH,
      ttlMs: RENDER_INDEX_TTL_MS,
    });
    this.onRenderObligationIndexed?.();
  }

  private async renderExecutionStream(input: {
    afterEventId: number;
    deliveryTimeoutMs?: number;
    executionId: string;
    sink: GoogleChatReplySink;
    stateThread?: Thread<GoogleChatThreadState>;
    threadKey: string;
    timeoutMs?: number;
  }): Promise<number> {
    let renderedText = '';
    let flushedText = '';
    let lastEventId = input.afterEventId;
    const stream = await withAbortTimeout(
      (signal) => this.sessionClient.streamEvents({
        afterEventId: input.afterEventId,
        executionId: input.executionId,
        onEventId: (eventId) => {
          lastEventId = Math.max(lastEventId, eventId);
        },
        signal,
        threadId: input.threadKey,
      }),
      input.timeoutMs,
      'open Google Chat render stream',
    );

    const mappedStream = chatSdkChunksToGoogleChatRenderChunks(harnessToChatSdkStream(stream));
    const iterator = conflateGoogleChatRenderStream(mappedStream)[Symbol.asyncIterator]();
    while (true) {
      const result = await nextWithTimeout(iterator, input.timeoutMs, 'render Google Chat stream');
      if (result.done) {
        break;
      }
      const chunk = result.value;
      if (chunk.type === 'error') {
        throw new Error(chunk.error);
      }
      if (chunk.type === 'text_delta') {
        renderedText += chunk.text;
        if (renderedText !== flushedText) {
          await this.persistSinkResult(
            input.threadKey,
            await withSinkTimeout(input.sink.emit(chunk.text, renderedText), input.deliveryTimeoutMs, 'update Google Chat render'),
            input.stateThread,
          );
          flushedText = renderedText;
        }
      }
      if (chunk.type === 'done') {
        break;
      }
    }

    const finalText = renderedText.trim() || 'Done.';
    await this.persistSinkResult(
      input.threadKey,
      await withSinkTimeout(input.sink.complete(finalText, renderedText), input.deliveryTimeoutMs, 'complete Google Chat render'),
      input.stateThread,
    );
    return lastEventId;
  }

  private async persistSinkResult(
    threadKey: string,
    result: GoogleChatReplySinkResult,
    thread?: Thread<GoogleChatThreadState>,
  ): Promise<void> {
    const progressMessageId = result?.progressMessageId;
    if (!progressMessageId) {
      return;
    }
    await this.withDistributedThreadTurnLock(threadKey, async () => {
      const latest = await this.getThreadState(threadKey, thread);
      const obligation = latest?.renderObligation;
      if (!obligation || obligation.progressMessageId === progressMessageId) {
        return;
      }
      await this.setThreadState(threadKey, {
        ...(latest ?? { active: true }),
        renderObligation: {
          ...obligation,
          progressMessageId,
        },
      }, thread);
    });
  }

  private async getThreadState(
    threadKey: string,
    thread?: Thread<GoogleChatThreadState>,
  ): Promise<GoogleChatThreadState | undefined> {
    if (thread) {
      return (await thread.state) ?? undefined;
    }
    return this.stateStore.get(threadKey);
  }

  private async setThreadState(
    threadKey: string,
    state: GoogleChatThreadState,
    thread?: Thread<GoogleChatThreadState>,
  ): Promise<void> {
    if (thread) {
      await thread.setState(state, { replace: true });
      return;
    }
    await this.stateStore.set(threadKey, state);
  }

  private createRecoverySink(
    threadKey: string,
    reference: StoredSpaceReference,
    messageId: string | undefined,
  ): GoogleChatReplySink | undefined {
    const injected = this.recoverySinkFactory?.(threadKey, reference, messageId);
    if (injected) {
      return injected;
    }
    if (this.gchatAdapter && threadKey.startsWith('gchat:')) {
      return createAdapterReplySink(this.gchatAdapter, threadKey, messageId, {
        minEditIntervalMs: this.config.gchat.renderMinEditIntervalMs,
      });
    }
    return undefined;
  }
}

export function toStoredSpaceReference(
  message: GoogleChatApiMessage,
  threadKey: string,
): StoredSpaceReference {
  return {
    isDm: isDirectMessageSpace(message, threadKey),
    spaceDisplayName: message.spaceDisplayName,
    spaceName: message.spaceName,
    spaceType: message.spaceType,
    threadName: message.threadName,
  };
}

async function* chatSdkChunksToGoogleChatRenderChunks(
  stream: AsyncIterable<ChatSDKStreamChunk>,
): AsyncIterable<GoogleChatRenderChunk> {
  for await (const chunk of stream) {
    if (chunk.type === 'markdown_text' && chunk.text) {
      yield { type: 'text_delta', text: chunk.text };
    }
  }
  yield { type: 'done' };
}

function appendUnique(values: string[] | undefined, value: string): string[] {
  return [...new Set([...(values ?? []), value])].slice(-1000);
}

function hasAppendInFlight(state: GoogleChatThreadState | undefined): boolean {
  return (state?.appendInFlight ?? 0) > 0;
}

function shouldAppendTurn(state: GoogleChatThreadState | undefined, activeExecutionTtlMs: number): boolean {
  return Boolean(state?.renderObligation)
    || hasLiveActiveExecution(state, activeExecutionTtlMs)
    || hasAppendInFlight(state);
}

function completionState(state: GoogleChatThreadState | undefined): Partial<GoogleChatThreadState> {
  if (hasAppendInFlight(state)) {
    return {
      ...(state ?? {}),
      activeExecution: true,
      activeExecutionStartedAt: state?.activeExecutionStartedAt ?? Date.now(),
      appendBarrier: true,
    };
  }
  return {
    ...(state ?? {}),
    activeExecution: false,
    activeExecutionStartedAt: null,
    appendBarrier: false,
  };
}

function samePendingRenderObligation(
  current: GoogleChatThreadState['renderObligation'] | undefined,
  expected: NonNullable<GoogleChatThreadState['renderObligation']>,
): boolean {
  return Boolean(
    current
      && !current.executionId
      && current.afterEventId === expected.afterEventId
      && current.message.id === expected.message.id
      && current.progressMessageId === expected.progressMessageId,
  );
}

async function failBestEffort(
  sink: GoogleChatReplySink,
  text: string,
  logger: Logger,
  timeoutMs?: number,
): Promise<void> {
  try {
    await withOptionalTimeout(sink.fail(text, ''), timeoutMs, 'fail Google Chat render');
  } catch (error) {
    logger.warn('gchat_render_error_update_failed', {
      error: error instanceof Error ? error.message : String(error),
    });
  }
}

export function hasLiveActiveExecution(
  state: GoogleChatThreadState | undefined,
  ttlMs: number,
  nowEpochMs = Date.now(),
): state is GoogleChatThreadState & { activeExecution: true; activeExecutionStartedAt: number } {
  if (state?.activeExecution !== true) return false;
  if (typeof state.activeExecutionStartedAt !== 'number') return false;
  return nowEpochMs - state.activeExecutionStartedAt <= ttlMs;
}

function isRetryableRecoveryError(error: unknown): boolean {
  if (error instanceof GoogleChatRenderDeliveryError) {
    return false;
  }
  if (error instanceof RecoveryTimeoutError) {
    return true;
  }
  if (error instanceof SessionApiError) {
    return error.retryable;
  }
  return true;
}

class GoogleChatRenderDeliveryError extends Error {
  constructor(action: string, cause: unknown) {
    super(`${action} failed: ${cause instanceof Error ? cause.message : String(cause)}`);
    this.name = 'GoogleChatRenderDeliveryError';
    this.cause = cause;
  }
}

class RecoveryTimeoutError extends Error {
  constructor(action: string, timeoutMs: number) {
    super(`${action} timed out after ${timeoutMs}ms`);
    this.name = 'RecoveryTimeoutError';
  }
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, action: string): Promise<T> {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        timeout = setTimeout(() => reject(new RecoveryTimeoutError(action, timeoutMs)), timeoutMs);
      }),
    ]);
  } finally {
    if (timeout) {
      clearTimeout(timeout);
    }
  }
}

function withOptionalTimeout<T>(promise: Promise<T>, timeoutMs: number | undefined, action: string): Promise<T> {
  return timeoutMs === undefined ? promise : withTimeout(promise, timeoutMs, action);
}

async function withSinkTimeout<T>(promise: Promise<T>, timeoutMs: number | undefined, action: string): Promise<T> {
  try {
    return await withOptionalTimeout(promise, timeoutMs, action);
  } catch (error) {
    if (error instanceof RecoveryTimeoutError) {
      throw error;
    }
    throw new GoogleChatRenderDeliveryError(action, error);
  }
}

async function withAbortTimeout<T>(
  operation: (signal?: AbortSignal) => Promise<T>,
  timeoutMs: number | undefined,
  action: string,
): Promise<T> {
  if (timeoutMs === undefined) {
    return operation();
  }
  const controller = new AbortController();
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      operation(controller.signal),
      new Promise<never>((_resolve, reject) => {
        timeout = setTimeout(() => {
          controller.abort();
          reject(new RecoveryTimeoutError(action, timeoutMs));
        }, timeoutMs);
      }),
    ]);
  } finally {
    if (timeout) {
      clearTimeout(timeout);
    }
  }
}

async function nextWithTimeout<T>(
  iterator: AsyncIterator<T>,
  timeoutMs: number | undefined,
  action: string,
): Promise<IteratorResult<T>> {
  if (timeoutMs === undefined) {
    return iterator.next();
  }
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      iterator.next(),
      new Promise<never>((_resolve, reject) => {
        timeout = setTimeout(() => {
          void iterator.return?.().catch(() => undefined);
          reject(new RecoveryTimeoutError(action, timeoutMs));
        }, timeoutMs);
      }),
    ]);
  } finally {
    if (timeout) {
      clearTimeout(timeout);
    }
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function isSpaceReferenceStore(
  value: GoogleChatThreadStateStore,
): value is GoogleChatThreadStateStore & SpaceReferenceStore {
  const candidate = value as Partial<SpaceReferenceStore>;
  return typeof candidate.getReference === 'function' && typeof candidate.setReference === 'function';
}

function isRenderRecoveryStateStore(
  value: GoogleChatThreadStateStore,
): value is GoogleChatRenderRecoveryStateStore {
  const candidate = value as Partial<GoogleChatRenderRecoveryStateStore>;
  return typeof candidate.acquireLiveRenderLease === 'function'
    && typeof candidate.acquireInboundMessageLease === 'function'
    && typeof candidate.acquireRenderRecoveryLease === 'function'
    && typeof candidate.indexRenderObligation === 'function'
    && typeof candidate.listRenderObligationThreadKeys === 'function';
}
