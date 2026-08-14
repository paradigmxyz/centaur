import { CodexAppServerRendererEventMapper } from '@centaur/rendering'
import { FeishuRenderer } from './feishu-client.js'
import {
  normalizeFeishuCardAction,
  normalizeFeishuMessage,
  type NormalizedFeishuCardAction,
  type NormalizedFeishuMessage
} from './feishu-events.js'
import {
  applySelectionAction,
  renderProgressCard,
  renderPublicationCard,
  renderSelectionCard,
  type SelectionAction,
  type SelectionCardState
} from './selection-cards.js'
import {
  FeishuApiError,
  FeishuSessionApi,
  type FeishuDelivery,
  type SelectionView
} from './session-api.js'
import { FeishuMetrics } from './metrics.js'

type UnknownRecord = Record<string, unknown>

export type FeishuBotOptions = {
  botOpenId: string
  tenantAllowlist: ReadonlySet<string>
  api: FeishuSessionApi
  renderer: FeishuRenderer
  metrics?: FeishuMetrics
  consolePublicUrl?: string
}

export class FeishuBot {
  private readonly metrics: FeishuMetrics
  private readonly activeRenders = new Map<string, Promise<void>>()
  private readonly activePublications = new Map<string, Promise<void>>()
  private readonly renderOwnerId = crypto.randomUUID()

  constructor(private readonly options: FeishuBotOptions) {
    this.metrics = options.metrics ?? new FeishuMetrics()
  }

  acceptMessageEvent(event: unknown): void {
    this.runEvent('message', async () => this.handleMessage(event))
  }

  acceptCardEvent(event: unknown): void {
    this.runEvent('card', async () => this.handleCard(event))
  }

  listPendingDeliveries(): Promise<string[]> {
    return this.options.api.listPendingDeliveries()
  }

  async reconcileDelivery(threadKey: string): Promise<void> {
    const delivery = await this.options.api.getDelivery(threadKey)
    if (delivery.selection_flow_id) {
      const view = await this.options.api.getSelection(
        delivery.selection_flow_id,
        delivery.initiator_principal_id
      )
      if (view.state === 'cancelled') {
        await this.renderDeliveryCard(
          delivery,
          renderProgressCard({ title: '任务已取消', status: '未启动代码执行' }),
          true
        )
        return
      }
      if (view.state === 'pending' || !delivery.execution_id) {
        const state = await this.selectionState(view)
        await this.renderDeliveryCard(delivery, renderSelectionCard(state), true)
        return
      }
    }
    if (delivery.publish_batch_id) {
      const batch = await this.options.api.getPublishBatch(
        delivery.publish_batch_id,
        delivery.initiator_principal_id
      ) as PublicationBatch
      const terminal = ['succeeded', 'partially_succeeded', 'failed'].includes(batch.state)
      const messageId = await this.renderDeliveryCard(delivery, renderPublicationCard(batch), terminal)
      if (!terminal && messageId) {
        await this.pollPublicationOnce(
          { ...delivery, message_id: messageId },
          batch.publish_batch_id,
          delivery.initiator_principal_id
        )
      }
      return
    }
    if (delivery.execution_id && delivery.message_id) {
      await this.renderExecutionOnce(delivery, delivery.message_id)
    } else if (delivery.execution_id) {
      const messageId = await this.renderDeliveryCard(
        delivery,
        renderProgressCard({ status: '正在处理' }),
        false
      )
      if (!messageId) return
      await this.renderExecutionOnce(delivery, messageId)
    }
  }

  private run(operation: () => Promise<void>): void {
    queueMicrotask(() => {
      void operation().catch(error => {
        const code = error instanceof FeishuApiError ? error.status : undefined
        console.error('feishubot operation failed', { code })
      })
    })
  }

  private runEvent(
    kind: 'message' | 'card',
    operation: () => Promise<boolean>
  ): void {
    const startedAt = performance.now()
    queueMicrotask(() => {
      void operation().then(accepted => {
        this.metrics.recordEvent(
          kind,
          accepted ? 'accepted' : 'ignored',
          performance.now() - startedAt
        )
      }).catch(error => {
        this.metrics.recordEvent(kind, 'failed', performance.now() - startedAt)
        const code = error instanceof FeishuApiError ? error.status : undefined
        console.error('feishubot operation failed', { code })
      })
    })
  }

  private async handleMessage(event: unknown): Promise<boolean> {
    const message = normalizeFeishuMessage(event, { botOpenId: this.options.botOpenId })
    if (!message || !this.options.tenantAllowlist.has(message.tenantKey)) return false
    if (message.command === 'new') {
      await this.startNewTask(message)
      return true
    }
    if (message.command === 'projects') {
      await this.addProjects(message)
      return true
    }
    const accepted = await this.options.api.acceptMessage(message)
    this.metrics.recordDeduplication(accepted.created ? 'new' : 'duplicate')
    const delivery = await this.options.api.getDelivery(accepted.thread_key)
    if (
      delivery.source_message_id !== message.messageId
      || delivery.execution_id !== accepted.execution_id
      || (accepted.selection_flow_id && delivery.selection_flow_id !== accepted.selection_flow_id)
    ) return true
    if (accepted.selection_flow_id) {
      await this.sendSelection(message, delivery, accepted.selection_flow_id)
      return true
    }
    const rendered = await this.upsertSessionCard(
      message,
      delivery,
      renderProgressCard({ status: '任务已提交' }),
      false
    )
    if (!rendered) return true
    const { messageId: cardMessageId, delivery: claimedDelivery } = rendered
    this.run(async () => this.renderExecutionOnce(claimedDelivery, cardMessageId))
    return true
  }

  private async startNewTask(message: NormalizedFeishuMessage): Promise<void> {
    try {
      await this.options.api.closeBinding(message)
      await this.options.renderer.replyText(message.messageId, '当前任务已结束。发送下一条消息开始新任务。', !message.isDirect)
    } catch (error) {
      if (error instanceof FeishuApiError && error.status === 404) {
        await this.options.renderer.replyText(message.messageId, '当前没有进行中的任务。', !message.isDirect)
        return
      }
      throw error
    }
  }

  private async addProjects(message: NormalizedFeishuMessage): Promise<void> {
    const binding = await this.options.api.activeBinding(message)
    let selection
    try {
      selection = await this.options.api.createAddSelection(
        binding.thread_key,
        message.principalId,
        message.messageId,
        message.eventId
      )
    } catch (error) {
      if (error instanceof FeishuApiError && error.status === 409) {
        await this.options.renderer.replyText(
          message.messageId,
          '上一张项目选择卡片仍在创建，请稍后重试。',
          !message.isDirect
        )
        return
      }
      throw error
    }
    const delivery = await this.options.api.getDelivery(binding.thread_key)
    if (
      delivery.source_message_id !== message.messageId
      || delivery.selection_flow_id !== selection.selection_flow_id
    ) return
    await this.sendSelection(message, delivery, selection.selection_flow_id)
  }

  private async sendSelection(
    message: NormalizedFeishuMessage,
    delivery: FeishuDelivery,
    selectionFlowId: string
  ): Promise<void> {
    const state = await this.loadSelectionState(selectionFlowId, message.principalId)
    await this.upsertSessionCard(message, delivery, renderSelectionCard(state), true)
  }

  private async upsertSessionCard(
    message: NormalizedFeishuMessage,
    delivery: FeishuDelivery,
    card: ReturnType<typeof renderProgressCard>,
    renderComplete: boolean
  ): Promise<{ messageId: string; delivery: FeishuDelivery } | undefined> {
    const claimed = await this.claimDelivery(delivery)
    if (!claimed) return undefined
    let messageId = claimed.message_id ?? undefined
    if (messageId) {
      await this.options.renderer.updateCard(messageId, card)
    } else {
      messageId = await this.options.renderer.replyCard(
        message.messageId,
        card,
        !message.isDirect,
        `${claimed.delivery_id}-${claimed.delivery_generation}`
      )
    }
    await this.recordDelivery(
      claimed.thread_key,
      messageId,
      claimed.last_event_cursor,
      claimed.desired_version,
      claimed.delivery_generation,
      renderComplete
    )
    return { messageId, delivery: claimed }
  }

  private async renderDeliveryCard(
    delivery: Awaited<ReturnType<FeishuSessionApi['getDelivery']>>,
    card: ReturnType<typeof renderProgressCard>,
    renderComplete: boolean
  ): Promise<string | undefined> {
    const claimed = await this.claimDelivery(delivery)
    if (!claimed) return undefined
    let messageId = claimed.message_id ?? undefined
    if (messageId) {
      await this.options.renderer.updateCard(messageId, card)
    } else {
      const sourceMessageId = claimed.source_message_id
      if (!sourceMessageId) throw new Error('Feishu delivery has no source message')
      messageId = await this.options.renderer.replyCard(
        sourceMessageId,
        card,
        claimed.root_message_id !== 'direct',
        `${claimed.delivery_id}-${claimed.delivery_generation}`
      )
    }
    await this.recordDelivery(
      claimed.thread_key,
      messageId,
      claimed.last_event_cursor,
      claimed.desired_version,
      claimed.delivery_generation,
      renderComplete
    )
    return messageId
  }

  private async handleCard(input: unknown): Promise<boolean> {
    const event = normalizeFeishuCardAction(input)
    if (!event || !this.options.tenantAllowlist.has(event.tenantKey)) return false
    const principalId = `feishu:${event.tenantKey}:${event.operatorOpenId}`
    if (event.action === 'approve_publication') {
      const changesetId = requiredOpaqueId(event.value.changeset_id, 'chg')
      const owningDelivery = await this.publicationDelivery(
        changesetId,
        principalId,
        event.messageId
      )
      if (!owningDelivery || !await this.claimDelivery(owningDelivery)) return true
      const batch = await this.options.api.approvePublication(
        changesetId,
        principalId,
        `feishu:${event.messageId}:${event.operatorOpenId}:approve:${changesetId}`
      ) as PublicationBatch
      const delivery = await this.options.api.getDelivery(owningDelivery.thread_key)
      if (!sameDeliveryGeneration(delivery, owningDelivery, event.messageId)) return true
      const messageId = await this.renderDeliveryCard(delivery, renderPublicationCard(batch), false)
      if (messageId) {
        this.run(async () => this.pollPublicationOnce(
          { ...delivery, message_id: messageId },
          batch.publish_batch_id,
          principalId
        ))
      }
      return true
    }
    if (event.action === 'retry_failed') {
      const batchId = requiredOpaqueId(event.value.publish_batch_id, 'pub')
      const previousBatch = await this.options.api.getPublishBatch(batchId, principalId) as PublicationBatch
      const owningDelivery = await this.publicationDelivery(
        previousBatch.changeset_id,
        principalId,
        event.messageId
      )
      if (
        !owningDelivery
        || owningDelivery.publish_batch_id !== batchId
        || !await this.claimDelivery(owningDelivery)
      ) return true
      const batch = await this.options.api.retryPublication(
        batchId,
        principalId,
        `feishu:${event.messageId}:${event.operatorOpenId}:retry:${batchId}:${event.eventId}`
      ) as PublicationBatch
      const delivery = await this.options.api.getDelivery(owningDelivery.thread_key)
      if (!sameDeliveryGeneration(delivery, owningDelivery, event.messageId)) return true
      const messageId = await this.renderDeliveryCard(delivery, renderPublicationCard(batch), false)
      if (messageId) {
        this.run(async () => this.pollPublicationOnce(
          { ...delivery, message_id: messageId },
          batch.publish_batch_id,
          principalId
        ))
      }
      return true
    }
    const selectionFlowId = requiredOpaqueId(event.value.selection_flow_id, 'sel')
    const expectedVersion = positiveInteger(event.value.expected_version)
    const view = await this.options.api.getSelection(selectionFlowId, principalId)
    const owningDelivery = await this.options.api.getDelivery(view.thread_key)
    if (
      owningDelivery.selection_flow_id !== selectionFlowId
      || owningDelivery.message_id !== event.messageId
    ) {
      this.metrics.recordStaleConflict('delivery')
      return true
    }
    if (view.version !== expectedVersion || view.state !== 'pending') {
      this.metrics.recordStaleConflict('selection')
      await this.renderDeliveryCard(
        owningDelivery,
        renderSelectionCard(await this.selectionState(view)),
        true
      )
      return true
    }
    if (event.action === 'confirm' || event.action === 'no_project') {
      await this.selectionMutation(() => event.action === 'confirm'
        ? this.options.api.confirmSelection(selectionFlowId, view.version, principalId, view.selected_repository_ids)
        : this.options.api.confirmNoProject(selectionFlowId, view.version, principalId))
      const delivery = await this.options.api.getDelivery(view.thread_key)
      if (!sameSelectionDelivery(delivery, owningDelivery, selectionFlowId, event.messageId)) return true
      const messageId = await this.renderDeliveryCard(
        delivery,
        renderProgressCard({ status: '工作区准备中' }),
        !view.execution_id
      )
      if (view.execution_id && messageId) {
        this.run(async () => this.renderExecutionOnce({
          ...delivery,
          execution_id: view.execution_id,
          initiator_principal_id: principalId
        }, messageId))
      }
      return true
    }
    if (event.action === 'cancel') {
      await this.selectionMutation(() => this.options.api.cancelSelection(
        selectionFlowId,
        view.version,
        principalId
      ))
      const delivery = await this.options.api.getDelivery(view.thread_key)
      if (!sameSelectionDelivery(delivery, owningDelivery, selectionFlowId, event.messageId)) return true
      await this.renderDeliveryCard(
        delivery,
        renderProgressCard({ title: '任务已取消', status: '未启动代码执行' }),
        true
      )
      return true
    }
    const current = await this.selectionState(view)
    const action = selectionAction(event, current)
    const next = applySelectionAction(current, action)
    const persisted = await this.selectionMutation(() => this.options.api.updateSelection(
      selectionFlowId,
      view.version,
      principalId,
      {
        query: next.query,
        cursor: next.cursor,
        cursor_history: next.cursorHistory,
        selected_repository_ids: next.selectedRepositoryIds
      }
    ))
    const delivery = await this.options.api.getDelivery(view.thread_key)
    if (!sameSelectionDelivery(delivery, owningDelivery, selectionFlowId, event.messageId)) return true
    await this.renderDeliveryCard(
      delivery,
      renderSelectionCard(await this.selectionState(persisted)),
      true
    )
    return true
  }

  private async loadSelectionState(selectionFlowId: string, principalId: string): Promise<SelectionCardState> {
    const view = await this.options.api.getSelection(selectionFlowId, principalId)
    return this.selectionState(view)
  }

  private async selectionState(view: SelectionView): Promise<SelectionCardState> {
    const page = await this.options.api.searchRepositories(view.query, view.cursor ?? undefined)
    return {
      selectionFlowId: view.selection_flow_id,
      expectedVersion: view.version,
      taskExcerpt: view.task_excerpt || (view.kind === 'add' ? '为当前任务添加项目' : '选择本次任务涉及的项目'),
      query: view.query,
      cursor: view.cursor ?? null,
      cursorHistory: view.cursor_history,
      nextCursor: page.next_cursor ?? null,
      selectedRepositoryIds: view.selected_repository_ids,
      repositories: page.repositories,
      status: view.state
    }
  }

  private renderExecutionOnce(delivery: FeishuDelivery, messageId: string): Promise<void> {
    const executionId = delivery.execution_id
    if (!executionId) return Promise.resolve()
    const key = `${delivery.thread_key}:${delivery.delivery_generation}:${executionId}`
    const active = this.activeRenders.get(key)
    if (active) return active
    const created = (async () => {
      try {
        const current = await this.options.api.getDelivery(delivery.thread_key)
        if (
          current.delivery_generation !== delivery.delivery_generation
          || current.execution_id !== executionId
          || current.message_id !== messageId
        ) return
        const claimed = await this.claimDelivery(current)
        if (!claimed) return
        await this.renderExecution(
          messageId,
          claimed.thread_key,
          executionId,
          claimed.initiator_principal_id,
          claimed.last_event_cursor,
          claimed.delivery_generation
        )
      } finally {
        this.activeRenders.delete(key)
      }
    })()
    this.activeRenders.set(key, created)
    return created
  }

  private async renderExecution(
    messageId: string,
    threadKey: string,
    executionId: string,
    principalId: string,
    afterEventId: number,
    deliveryGeneration: number
  ): Promise<void> {
    const mapper = new CodexAppServerRendererEventMapper({ sessionId: threadKey })
    let status = '正在处理'
    let changesetId: string | undefined
    let lastRender = 0
    let lastEventCursor = afterEventId
    for await (const event of this.options.api.streamEvents(threadKey, executionId, 0)) {
      const executionFailed = event.event === 'session.execution_failed'
      if (event.id !== undefined) lastEventCursor = event.id
      if (event.event === 'development.changeset_ready') {
        changesetId = record(event.data)?.changeset_id as string | undefined
      } else if (event.event === 'development.changeset_empty') {
        status = '任务完成，没有代码变更'
      } else if (event.event === 'development.changeset_needs_agent_completion') {
        status = '代码修改需要继续处理后才能发布'
      } else if (event.event === 'development.changeset_failed') {
        status = '无法生成可审查的修改'
      } else {
        const mapped = mapper.process({ eventKind: event.event, data: event.data, eventId: event.id })
        const latestStatus = [...mapped].reverse().find(item => item.type === 'renderer.status')
        if (latestStatus?.type === 'renderer.status') status = latestStatus.status
        if (executionFailed) status = string(record(event.data)?.error) || '任务执行失败'
      }
      const terminal = Boolean(changesetId)
        || executionFailed
        || ['development.changeset_empty', 'development.changeset_needs_agent_completion', 'development.changeset_failed'].includes(event.event)
      const isAfterCursor = event.id === undefined
        ? afterEventId === 0
        : event.id > afterEventId
      if (isAfterCursor && (terminal || Date.now() - lastRender >= 1_200)) {
        lastRender = Date.now()
        const current = await this.options.api.getDelivery(threadKey)
        if (
          current.delivery_generation !== deliveryGeneration
          || current.execution_id !== executionId
          || current.message_id !== messageId
        ) return
        const claimed = await this.claimDelivery(current)
        if (!claimed) return
        await this.options.renderer.updateCard(messageId, renderProgressCard({
          status,
          answer: mapper.answerText(),
          ...(changesetId ? {
            changesetId,
            consoleUrl: this.changeSetUrl(changesetId, principalId)
          } : {}),
          failed: executionFailed || event.event === 'development.changeset_failed'
        }))
        const recorded = await this.recordDelivery(
          threadKey,
          messageId,
          lastEventCursor,
          claimed.desired_version,
          deliveryGeneration,
          terminal
        )
        if (!recorded) return
      }
      if (terminal && isAfterCursor) return
    }
  }

  private changeSetUrl(changesetId: string, _principalId: string): string | undefined {
    if (!this.options.consolePublicUrl) return undefined
    return `${this.options.consolePublicUrl}/console/changesets/${encodeURIComponent(changesetId)}`
  }

  private async selectionMutation<T>(operation: () => Promise<T>): Promise<T> {
    try {
      return await operation()
    } catch (error) {
      if (error instanceof FeishuApiError && error.status === 409) {
        this.metrics.recordStaleConflict('selection')
      }
      throw error
    }
  }

  private async claimDelivery(delivery: FeishuDelivery): Promise<FeishuDelivery | undefined> {
    try {
      return await this.options.api.claimDelivery(
        delivery.thread_key,
        delivery.delivery_generation,
        delivery.desired_version,
        this.renderOwnerId
      )
    } catch (error) {
      if (!(error instanceof FeishuApiError) || error.status !== 409) throw error
      this.metrics.recordStaleConflict('delivery')
      return undefined
    }
  }

  private async recordDelivery(
    threadKey: string,
    messageId: string,
    lastEventCursor: number,
    desiredVersion: number,
    deliveryGeneration: number,
    renderComplete: boolean
  ): Promise<boolean> {
    try {
      await this.options.api.recordDelivery(
        threadKey,
        messageId,
        lastEventCursor,
        desiredVersion,
        deliveryGeneration,
        this.renderOwnerId,
        renderComplete
      )
      return true
    } catch (error) {
      if (!(error instanceof FeishuApiError) || error.status !== 409) throw error
      this.metrics.recordStaleConflict('delivery')
      return false
    }
  }

  private async publicationDelivery(
    changesetId: string,
    principalId: string,
    messageId: string
  ): Promise<FeishuDelivery | undefined> {
    const changeset = await this.options.api.getChangeset(changesetId, principalId)
    const threadKey = string(record(changeset)?.thread_key)
    if (!threadKey) throw new Error('publication changeset has no session')
    const delivery = await this.options.api.getDelivery(threadKey)
    if (delivery.message_id === messageId) return delivery
    this.metrics.recordStaleConflict('delivery')
    return undefined
  }

  private pollPublicationOnce(
    delivery: FeishuDelivery,
    batchId: string,
    principalId: string
  ): Promise<void> {
    const key = `${delivery.thread_key}:${delivery.delivery_generation}:${batchId}`
    const active = this.activePublications.get(key)
    if (active) return active
    const created = (async () => {
      try {
        await this.pollPublication(delivery, batchId, principalId)
      } finally {
        this.activePublications.delete(key)
      }
    })()
    this.activePublications.set(key, created)
    return created
  }

  private async pollPublication(
    expectedDelivery: FeishuDelivery,
    batchId: string,
    principalId: string
  ): Promise<void> {
    for (let attempt = 0; attempt < 150; attempt += 1) {
      await Bun.sleep(2_000)
      const batch = await this.options.api.getPublishBatch(batchId, principalId) as PublicationBatch
      const delivery = await this.options.api.getDelivery(expectedDelivery.thread_key)
      if (
        !sameDeliveryGeneration(delivery, expectedDelivery, expectedDelivery.message_id ?? '')
        || delivery.publish_batch_id !== batchId
      ) return
      const terminal = ['succeeded', 'partially_succeeded', 'failed'].includes(batch.state)
      const messageId = await this.renderDeliveryCard(
        delivery,
        renderPublicationCard(batch),
        terminal
      )
      if (!messageId || terminal) return
    }
    throw new Error('publication status polling timed out')
  }
}

type PublicationBatch = {
  publish_batch_id: string
  changeset_id: string
  state: string
  items: Array<{
    repository_id: string
    state: string
    merge_request_url?: string | null
    failure_message?: string | null
  }>
}

function selectionAction(event: NormalizedFeishuCardAction, state: SelectionCardState): SelectionAction {
  const base = { expectedVersion: state.expectedVersion }
  switch (event.action) {
    case 'toggle':
      return { action: 'toggle', repositoryId: requiredRepositoryId(event.value.repository_id), ...base }
    case 'remove':
      return { action: 'remove', repositoryId: requiredRepositoryId(event.value.repository_id), ...base }
    case 'search':
      return { action: 'search', query: string(event.formValue.query) ?? '', ...base }
    case 'next': return { action: 'next', ...base }
    case 'previous': return { action: 'previous', ...base }
    default: throw new Error('unsupported Feishu card action')
  }
}

function sameSelectionDelivery(
  current: FeishuDelivery,
  previous: FeishuDelivery,
  selectionFlowId: string,
  messageId: string
): boolean {
  return current.delivery_generation === previous.delivery_generation
    && current.selection_flow_id === selectionFlowId
    && current.message_id === messageId
}

function sameDeliveryGeneration(
  current: FeishuDelivery,
  previous: FeishuDelivery,
  messageId: string
): boolean {
  return current.delivery_generation === previous.delivery_generation
    && current.message_id === messageId
}

function requiredRepositoryId(value: unknown): string {
  const id = string(value)
  if (!id || !/^gitlab:[1-9][0-9]*$/u.test(id)) throw new Error('invalid repository ID')
  return id
}

function requiredOpaqueId(value: unknown, prefix: string): string {
  const id = string(value)
  if (!id || !new RegExp(`^${prefix}_[A-Za-z0-9-]+$`, 'u').test(id)) throw new Error('invalid opaque ID')
  return id
}

function positiveInteger(value: unknown): number {
  if (!Number.isInteger(value) || Number(value) < 1) throw new Error('invalid expected version')
  return Number(value)
}

function record(input: unknown): UnknownRecord | undefined {
  return input !== null && typeof input === 'object' && !Array.isArray(input)
    ? input as UnknownRecord
    : undefined
}

function string(input: unknown): string | undefined {
  return typeof input === 'string' && input.trim() ? input.trim() : undefined
}
