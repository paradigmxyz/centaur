import { codexAppServerToChatSdkStream, type ChatSDKStreamChunk } from '@centaur/rendering';
import type { TeamsAdapter } from '@chat-adapter/teams';
import type { Logger, Message as ChatMessage, Thread } from 'chat';
import type { TeamsbotConfig } from './config.js';
import { createTeamsbotLogger } from './logger.js';
import { conflateTeamsRenderStream, type TeamsRenderChunk } from './conflate.js';
import { toStoredConversationReference } from './conversation-reference.js';
import { createGraphTokenProvider, type GraphTokenProvider } from './graph-token.js';
import { createAdapterBlockReplySink, createChatReplySink, type TeamsReplySink, type TeamsReplySinkResult } from './reply-sink.js';
import { CentaurSessionClient, SessionApiError } from './session-api.js';
import type {
  ConversationReferenceStore,
  StoredConversationReference,
  TeamsActivity,
  TeamsApiMessage,
  TeamsRenderRecoveryStateStore,
  TeamsThreadState,
  TeamsThreadStateStore,
} from './types.js';
import { hydrateTeamsAttachments } from './teams-attachments.js';
import {
  isAllowedTeamsActivity,
  serializeTeamsMessage,
  teamsQuotedReplyContextMessages,
} from './teams-message.js';

const RENDER_INDEX_TTL_MS = 7 * 24 * 60 * 60 * 1000;
const RENDER_OBLIGATION_INDEX_MAX_LENGTH = 2000;
const RENDER_RECOVERY_LEASE_TTL_MS = 2 * 60 * 1000;
const RENDER_RECOVERY_LEASE_TIMEOUT_SAFETY_MS = 1_000;
const INBOUND_MESSAGE_LEASE_TTL_MS = 30 * 60 * 1000;
const THREAD_TURN_LEASE_TTL_MS = 30_000;
const ActivityTypes = {
  Message: 'message',
} as const;

type TeamsbotServiceOptions = {
  conversationReferenceStore?: ConversationReferenceStore;
  graphTokenProvider?: GraphTokenProvider;
  logger?: Logger;
  onRenderObligationIndexed?: () => void;
  recoverySinkFactory?: (reference: StoredConversationReference, activityId?: string) => TeamsReplySink | undefined;
  teamsAdapter?: TeamsAdapter;
};

export class TeamsbotService {
  private readonly sessionClient: CentaurSessionClient;
  private readonly conversationReferenceStore?: ConversationReferenceStore;
  private readonly graphTokenProvider: GraphTokenProvider;
  readonly logger: Logger;
  private readonly onRenderObligationIndexed?: () => void;
  private readonly recoverySinkFactory?: (reference: StoredConversationReference, activityId?: string) => TeamsReplySink | undefined;
  private readonly recoveryStateStore?: TeamsRenderRecoveryStateStore;
  private readonly teamsAdapter?: TeamsAdapter;
  private readonly threadLocks = new Map<string, Promise<void>>();

  constructor(
    private readonly config: TeamsbotConfig,
    private readonly stateStore: TeamsThreadStateStore,
    sessionClient?: CentaurSessionClient,
    options: TeamsbotServiceOptions = {},
  ) {
    this.logger = options.logger ?? createTeamsbotLogger(config.server.logLevel);
    this.sessionClient = sessionClient ?? new CentaurSessionClient({
      apiKey: config.centaur.apiKey,
      apiUrl: config.centaur.apiUrl,
      defaultHarnessType: config.teams.defaultHarnessType,
      idleTimeoutMs: config.teams.idleTimeoutMs,
      logger: this.logger.child('session-api'),
      maxDurationMs: config.teams.maxDurationMs,
      requestMaxRetries: config.centaur.requestMaxRetries,
      requestRetryDelayMs: config.centaur.requestRetryDelayMs,
    });
    this.conversationReferenceStore = options.conversationReferenceStore
      ?? (isConversationReferenceStore(stateStore) ? stateStore : undefined);
    this.recoveryStateStore = isRenderRecoveryStateStore(stateStore) ? stateStore : undefined;
    this.graphTokenProvider = options.graphTokenProvider ?? createGraphTokenProvider(config);
    this.onRenderObligationIndexed = options.onRenderObligationIndexed;
    this.recoverySinkFactory = options.recoverySinkFactory;
    this.teamsAdapter = options.teamsAdapter;
  }

  async runChatMessage(
    thread: Thread<TeamsThreadState>,
    chatMessage: ChatMessage,
    mode: 'append' | 'execute',
  ): Promise<void> {
    const activity = chatMessage.raw as TeamsActivity | undefined;
    if (!activity || typeof activity !== 'object') {
      return;
    }
    await this.handleChatActivity(thread, chatMessage, activity, mode);
  }

  private async handleChatActivity(
    thread: Thread<TeamsThreadState>,
    chatMessage: ChatMessage,
    activity: TeamsActivity,
    mode: 'append' | 'execute',
  ): Promise<void> {
    if (!activity || activity.type !== ActivityTypes.Message) {
      return;
    }
    if (!isAllowedTeamsActivity({
      activity,
      allowedChannelIds: this.config.teams.allowedChannelIds,
      allowedTeamIds: this.config.teams.allowedTeamIds,
      allowedTenantIds: this.config.teams.allowedTenantIds,
    })) {
      return;
    }
    if (this.isFromBot(activity)) {
      return;
    }

    const activityContext = { activity };
    const text = chatMessage.text;
    const threadKey = thread.id;
    await this.conversationReferenceStore?.setReference(threadKey, toStoredConversationReference(activity));
    const existing = await this.getThreadState(threadKey, thread);
    const mentioned = chatMessage.isMention === true;
    const active = existing?.active === true || mentioned || mode === 'execute' || !this.config.teams.requireMention;
    if (!active) {
      return;
    }

    const serialized = serializeTeamsMessage(activityContext, threadKey, text);
    serialized.isMention = mentioned;
    const contextMessages = teamsQuotedReplyContextMessages(activity, threadKey);
    const message: TeamsApiMessage = {
      ...serialized,
      attachments: await hydrateTeamsAttachments(serialized.attachments, {
        allowedHosts: this.config.teams.attachmentAllowedHosts,
        enabled: this.config.teams.attachmentDownloadEnabled,
        graphTokenProvider: this.graphTokenProvider,
        graphTokenScope: this.config.teams.graphTokenScope,
        maxBytes: this.config.teams.attachmentMaxBytes,
      }),
    };
    if (!text && message.attachments.length === 0) {
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
            forwardedMessageIds: appendUnique(latest?.forwardedMessageIds ?? action.existing?.forwardedMessageIds, message.id),
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
        contextMessages,
        createChatReplySink(thread, activity.conversation?.conversationType),
      );
    } finally {
      await action.releaseInboundMessageLease?.().catch(() => undefined);
    }
  }

  private async claimTurnAction(
    thread: Thread<TeamsThreadState>,
    threadKey: string,
    existing: TeamsThreadState | undefined,
    messageId: string,
    options: { forceAppend?: boolean } = {},
  ): Promise<
    {
      existing: TeamsThreadState | undefined;
      kind: 'append' | 'execute' | 'noop';
      releaseInboundMessageLease?: () => Promise<void>;
      releaseThreadTurnLease?: () => Promise<void>;
    }
  > {
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
        return this.claimTurnActionUnderLease(thread, threadKey, latest, existing, releaseInboundLease, releaseThreadTurnLease, options);
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
    thread: Thread<TeamsThreadState>,
    threadKey: string,
    latest: TeamsThreadState | undefined,
    observed: TeamsThreadState | undefined,
    releaseInboundMessageLease: (() => Promise<void>) | undefined,
    releaseThreadTurnLease: (() => Promise<void>) | undefined,
    options: { forceAppend?: boolean } = {},
  ): Promise<
    {
      existing: TeamsThreadState | undefined;
      kind: 'append' | 'execute' | 'noop';
      releaseInboundMessageLease?: () => Promise<void>;
      releaseThreadTurnLease?: () => Promise<void>;
    }
  > {
    if (options.forceAppend
      || shouldAppendTurn(latest, this.config.teams.activeExecutionTtlMs)
      || shouldAppendTurn(observed, this.config.teams.activeExecutionTtlMs)) {
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
        throw new Error('Teams thread turn lease is unavailable');
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

  private async finishAppendClaim(thread: Thread<TeamsThreadState>, threadKey: string): Promise<void> {
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
    thread: Thread<TeamsThreadState>,
    threadKey: string,
    message: TeamsApiMessage,
    existing: Awaited<ReturnType<TeamsThreadStateStore['get']>>,
    contextMessages: TeamsApiMessage[],
    sink: TeamsReplySink,
  ): Promise<void> {
    let progressActivityId: string | undefined;
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
      ({ progressActivityId } = await withTimeout(sink.begin(), this.config.teams.activeExecutionTtlMs, 'begin Teams live render'));
      await this.sessionClient.createSession(threadKey, message);
      const alreadyForwarded = new Set(existing?.forwardedMessageIds ?? []);
      if (!alreadyForwarded.has(message.id)) {
        await this.sessionClient.appendMessages(threadKey, [message]);
      }
      releaseRenderLease = await this.recoveryStateStore?.acquireLiveRenderLease(threadKey, RENDER_RECOVERY_LEASE_TTL_MS) ?? null;
      if (!releaseRenderLease) {
        throw new Error('Teams render lease is unavailable');
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
            contextMessages,
            message,
            progressActivityId,
          },
        }, thread);
      });
      await this.indexRenderObligation(threadKey);
      const execution = await withAbortTimeout(
        (signal) => this.sessionClient.executeSession(threadKey, message, contextMessages, { signal }),
        this.config.teams.activeExecutionTtlMs,
        'execute Teams live session',
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
            contextMessages,
            executionId: execution.execution_id,
            message,
            progressActivityId,
          },
        }, thread);
      });

      lastEventId = await this.renderExecutionStream({
        afterEventId: lastEventId,
        deliveryTimeoutMs: this.config.teams.renderDeliveryTimeoutMs,
        executionId: execution.execution_id,
        sink,
        stateThread: thread,
        threadKey,
        timeoutMs: this.config.teams.activeExecutionTtlMs,
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
        await failBestEffort(sink, `Error: ${messageText}`, this.logger, this.config.teams.activeExecutionTtlMs);
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
        this.logger.warn('teams_render_recovery_lease_skipped', { threadKey });
        continue;
      }
      const state = await this.getThreadState(threadKey);
      if (!state?.renderObligation) {
        await releaseLease();
        continue;
      }
      const obligation = state.renderObligation;
      const storedReference = await this.conversationReferenceStore?.getReference(threadKey);
      if (!storedReference) {
        this.logger.warn('teams_render_recovery_skipped_no_reference', { threadKey });
        await this.clearRenderObligation(threadKey);
        await releaseLease();
        continue;
      }
      const sink = this.createRecoverySink(threadKey, storedReference, obligation.progressActivityId);
      if (!sink) {
        this.logger.warn('teams_render_recovery_skipped_no_sdk_app', { threadKey });
        await this.clearRenderObligation(threadKey);
        await releaseLease();
        continue;
      }
      const timeoutMs = this.recoveryAttemptTimeoutMs();
      try {
        await withTimeout(sink.begin(), timeoutMs, 'begin Teams recovery render');
        const executionId = await this.ensureRecoveryExecutionId(threadKey, obligation, timeoutMs);
        await this.persistRecoveryExecutionId(
          threadKey,
          obligation,
          executionId,
        );
        const lastEventId = await this.renderExecutionStream({
          afterEventId: obligation.afterEventId,
          deliveryTimeoutMs: this.config.teams.renderDeliveryTimeoutMs,
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
        this.logger.error('teams_render_recovery_failed', { error, threadKey });
      } finally {
        await releaseLease().catch(() => undefined);
      }
    }
    return deferredCount;
  }

  private async ensureRecoveryExecutionId(
    threadKey: string,
    obligation: NonNullable<TeamsThreadState['renderObligation']>,
    timeoutMs: number,
  ): Promise<string> {
    if (obligation.executionId) {
      return obligation.executionId;
    }
    const execution = await withAbortTimeout(
      (signal) => this.sessionClient.executeSession(threadKey, obligation.message, obligation.contextMessages, { signal }),
      timeoutMs,
      'execute Teams recovery session',
    );
    return execution.execution_id;
  }

  private async persistRecoveryExecutionId(
    threadKey: string,
    obligation: NonNullable<TeamsThreadState['renderObligation']>,
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
        this.config.teams.activeExecutionTtlMs,
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
    sink: TeamsReplySink;
    stateThread?: Thread<TeamsThreadState>;
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
      'open Teams recovery stream',
    );

    const mappedStream = chatSdkChunksToTeamsRenderChunks(codexAppServerToChatSdkStream(stream));

    const iterator = conflateTeamsRenderStream(mappedStream)[Symbol.asyncIterator]();
    while (true) {
      const result = await nextWithTimeout(iterator, input.timeoutMs, 'render Teams recovery stream');
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
            await withSinkTimeout(input.sink.emit(chunk.text, renderedText), input.deliveryTimeoutMs, 'update Teams render'),
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
      await withSinkTimeout(input.sink.complete(finalText, renderedText), input.deliveryTimeoutMs, 'complete Teams render'),
      input.stateThread,
    );
    return lastEventId;
  }

  private async persistSinkResult(
    threadKey: string,
    result: TeamsReplySinkResult,
    thread?: Thread<TeamsThreadState>,
  ): Promise<void> {
    const progressActivityId = result?.progressActivityId;
    if (!progressActivityId) {
      return;
    }
    await this.withDistributedThreadTurnLock(threadKey, async () => {
      const latest = await this.getThreadState(threadKey, thread);
      const obligation = latest?.renderObligation;
      if (!obligation || obligation.progressActivityId === progressActivityId) {
        return;
      }
      await this.setThreadState(threadKey, {
        ...(latest ?? { active: true }),
        renderObligation: {
          ...obligation,
          progressActivityId,
        },
      }, thread);
    });
  }

  private isFromBot(activity: TeamsActivity): boolean {
    const fromId = activity.from?.id;
    const recipientId = activity.recipient?.id;
    return Boolean(fromId && recipientId && fromId.toLowerCase() === recipientId.toLowerCase());
  }

  private async getThreadState(
    threadKey: string,
    thread?: Thread<TeamsThreadState>,
  ): Promise<TeamsThreadState | undefined> {
    if (thread) {
      return (await thread.state) ?? undefined;
    }
    return this.stateStore.get(threadKey);
  }

  private async setThreadState(
    threadKey: string,
    state: TeamsThreadState,
    thread?: Thread<TeamsThreadState>,
  ): Promise<void> {
    if (thread) {
      await thread.setState(state, { replace: true });
      return;
    }
    await this.stateStore.set(threadKey, state);
  }

  private createRecoverySink(threadKey: string, reference: StoredConversationReference, activityId: string | undefined): TeamsReplySink | undefined {
    const injected = this.recoverySinkFactory?.(reference, activityId);
    if (injected) {
      return injected;
    }
    if (this.teamsAdapter && threadKey.startsWith('teams:')) {
      return createAdapterBlockReplySink(this.teamsAdapter, threadKey, activityId);
    }
    return undefined;
  }
}

async function* chatSdkChunksToTeamsRenderChunks(
  stream: AsyncIterable<ChatSDKStreamChunk>,
): AsyncIterable<TeamsRenderChunk> {
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

function hasAppendInFlight(state: TeamsThreadState | undefined): boolean {
  return (state?.appendInFlight ?? 0) > 0;
}

function shouldAppendTurn(state: TeamsThreadState | undefined, activeExecutionTtlMs: number): boolean {
  return Boolean(state?.renderObligation)
    || hasLiveActiveExecution(state, activeExecutionTtlMs)
    || hasAppendInFlight(state);
}

function completionState(state: TeamsThreadState | undefined): Partial<TeamsThreadState> {
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
  current: TeamsThreadState['renderObligation'] | undefined,
  expected: NonNullable<TeamsThreadState['renderObligation']>,
): boolean {
  return Boolean(
    current
      && !current.executionId
      && current.afterEventId === expected.afterEventId
      && current.message.id === expected.message.id
      && current.progressActivityId === expected.progressActivityId,
  );
}

async function failBestEffort(sink: TeamsReplySink, text: string, logger: Logger, timeoutMs?: number): Promise<void> {
  try {
    await withOptionalTimeout(sink.fail(text, ''), timeoutMs, 'fail Teams render');
  } catch (error) {
    logger.warn('teams_render_error_update_failed', {
      error: error instanceof Error ? error.message : String(error),
    });
  }
}

export function hasLiveActiveExecution(
  state: TeamsThreadState | undefined,
  ttlMs: number,
  nowEpochMs = Date.now(),
): state is TeamsThreadState & { activeExecution: true; activeExecutionStartedAt: number } {
  if (state?.activeExecution !== true) return false;
  if (typeof state.activeExecutionStartedAt !== 'number') return false;
  return nowEpochMs - state.activeExecutionStartedAt <= ttlMs;
}

function isRetryableRecoveryError(error: unknown): boolean {
  if (error instanceof TeamsRenderDeliveryError) {
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

class TeamsRenderDeliveryError extends Error {
  constructor(action: string, cause: unknown) {
    super(`${action} failed: ${cause instanceof Error ? cause.message : String(cause)}`);
    this.name = 'TeamsRenderDeliveryError';
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
    throw new TeamsRenderDeliveryError(action, error);
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

function isConversationReferenceStore(value: TeamsThreadStateStore): value is TeamsThreadStateStore & ConversationReferenceStore {
  const candidate = value as Partial<ConversationReferenceStore>;
  return typeof candidate.getReference === 'function' && typeof candidate.setReference === 'function';
}

function isRenderRecoveryStateStore(value: TeamsThreadStateStore): value is TeamsRenderRecoveryStateStore {
  const candidate = value as Partial<TeamsRenderRecoveryStateStore>;
  return typeof candidate.acquireLiveRenderLease === 'function'
    && typeof candidate.acquireInboundMessageLease === 'function'
    && typeof candidate.acquireRenderRecoveryLease === 'function'
    && typeof candidate.indexRenderObligation === 'function'
    && typeof candidate.listRenderObligationThreadKeys === 'function';
}
