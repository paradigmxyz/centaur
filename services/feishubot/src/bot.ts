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
import { FeishuApiError, FeishuSessionApi, type SelectionView } from './session-api.js'
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
      const state = await this.loadSelectionState(
        delivery.selection_flow_id,
        delivery.initiator_principal_id
      )
      await this.renderDeliveryCard(delivery, renderSelectionCard(state))
      return
    }
    if (delivery.publish_batch_id) {
      const batch = await this.options.api.getPublishBatch(
        delivery.publish_batch_id,
        delivery.initiator_principal_id
      ) as PublicationBatch
      await this.renderDeliveryCard(delivery, renderPublicationCard(batch))
      if (!['succeeded', 'partially_succeeded', 'failed'].includes(batch.state)) {
        this.run(async () => this.pollPublication(
          delivery.message_id!,
          batch.publish_batch_id,
          delivery.initiator_principal_id
        ))
      }
      return
    }
    if (delivery.execution_id && delivery.message_id) {
      this.run(async () => this.renderExecution(
        delivery.message_id!,
        delivery.thread_key,
        delivery.execution_id!,
        delivery.initiator_principal_id,
        delivery.last_event_cursor
      ))
    } else if (delivery.execution_id) {
      const messageId = await this.renderDeliveryCard(
        delivery,
        renderProgressCard({ status: '正在处理' })
      )
      this.run(async () => this.renderExecution(
        messageId,
        delivery.thread_key,
        delivery.execution_id!,
        delivery.initiator_principal_id,
        delivery.last_event_cursor
      ))
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
    if (accepted.selection_flow_id) {
      await this.sendSelection(message, accepted.thread_key, accepted.selection_flow_id)
      return true
    }
    const cardMessageId = await this.upsertSessionCard(
      message,
      accepted.thread_key,
      renderProgressCard({ status: '任务已提交' })
    )
    this.run(async () => this.renderExecution(
      cardMessageId,
      accepted.thread_key,
      accepted.execution_id,
      message.principalId
    ))
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
    const selection = await this.options.api.createAddSelection(binding.thread_key, message.principalId)
    await this.sendSelection(message, binding.thread_key, selection.selection_flow_id)
  }

  private async sendSelection(
    message: NormalizedFeishuMessage,
    threadKey: string,
    selectionFlowId: string
  ): Promise<void> {
    const state = await this.loadSelectionState(selectionFlowId, message.principalId)
    await this.upsertSessionCard(message, threadKey, renderSelectionCard(state))
  }

  private async upsertSessionCard(
    message: NormalizedFeishuMessage,
    threadKey: string,
    card: ReturnType<typeof renderProgressCard>
  ): Promise<string> {
    const delivery = await this.options.api.getDelivery(threadKey)
    let messageId = delivery.message_id ?? undefined
    if (messageId) {
      await this.options.renderer.updateCard(messageId, card)
    } else {
      messageId = await this.options.renderer.replyCard(
        message.messageId,
        card,
        !message.isDirect,
        delivery.delivery_id
      )
    }
    await this.recordDelivery(
      threadKey,
      messageId,
      delivery.last_event_cursor,
      delivery.desired_version
    )
    return messageId
  }

  private async renderDeliveryCard(
    delivery: Awaited<ReturnType<FeishuSessionApi['getDelivery']>>,
    card: ReturnType<typeof renderProgressCard>
  ): Promise<string> {
    let messageId = delivery.message_id ?? undefined
    if (messageId) {
      await this.options.renderer.updateCard(messageId, card)
    } else {
      const sourceMessageId = delivery.source_message_id
      if (!sourceMessageId) throw new Error('Feishu delivery has no source message')
      messageId = await this.options.renderer.replyCard(
        sourceMessageId,
        card,
        delivery.root_message_id !== 'direct',
        delivery.delivery_id
      )
    }
    await this.recordDelivery(
      delivery.thread_key,
      messageId,
      delivery.last_event_cursor,
      delivery.desired_version
    )
    return messageId
  }

  private async handleCard(input: unknown): Promise<boolean> {
    const event = normalizeFeishuCardAction(input)
    if (!event || !this.options.tenantAllowlist.has(event.tenantKey)) return false
    const principalId = `feishu:${event.tenantKey}:${event.operatorOpenId}`
    if (event.action === 'approve_publication') {
      const changesetId = requiredOpaqueId(event.value.changeset_id, 'chg')
      const batch = await this.options.api.approvePublication(
        changesetId,
        principalId,
        `feishu:${event.messageId}:${event.operatorOpenId}:approve:${changesetId}`
      ) as PublicationBatch
      await this.options.renderer.updateCard(event.messageId, renderPublicationCard(batch))
      this.run(async () => this.pollPublication(event.messageId, batch.publish_batch_id, principalId))
      return true
    }
    if (event.action === 'retry_failed') {
      const batchId = requiredOpaqueId(event.value.publish_batch_id, 'pub')
      const batch = await this.options.api.retryPublication(
        batchId,
        principalId,
        `feishu:${event.messageId}:${event.operatorOpenId}:retry:${batchId}:${event.eventId}`
      ) as PublicationBatch
      await this.options.renderer.updateCard(event.messageId, renderPublicationCard(batch))
      this.run(async () => this.pollPublication(event.messageId, batch.publish_batch_id, principalId))
      return true
    }
    const selectionFlowId = requiredOpaqueId(event.value.selection_flow_id, 'sel')
    const expectedVersion = positiveInteger(event.value.expected_version)
    const view = await this.options.api.getSelection(selectionFlowId, principalId)
    if (view.version !== expectedVersion || view.state !== 'pending') {
      this.metrics.recordStaleConflict('selection')
      await this.refreshSelection(event.messageId, view, principalId)
      return true
    }
    if (event.action === 'confirm' || event.action === 'no_project') {
      await this.selectionMutation(() => event.action === 'confirm'
        ? this.options.api.confirmSelection(selectionFlowId, view.version, principalId, view.selected_repository_ids)
        : this.options.api.confirmNoProject(selectionFlowId, view.version, principalId))
      await this.options.renderer.updateCard(event.messageId, renderProgressCard({ status: '工作区准备中' }))
      const delivery = await this.options.api.getDelivery(view.thread_key)
      await this.recordDelivery(
        view.thread_key,
        event.messageId,
        delivery.last_event_cursor,
        delivery.desired_version
      )
      if (view.execution_id) {
        this.run(async () => this.renderExecution(
          event.messageId,
          view.thread_key,
          view.execution_id!,
          principalId
        ))
      }
      return true
    }
    if (event.action === 'cancel') {
      await this.selectionMutation(() => this.options.api.cancelSelection(
        selectionFlowId,
        view.version,
        principalId
      ))
      await this.options.renderer.updateCard(
        event.messageId,
        renderProgressCard({ title: '任务已取消', status: '未启动代码执行' })
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
    await this.refreshSelection(event.messageId, persisted, principalId)
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

  private async refreshSelection(messageId: string, view: SelectionView, _principalId: string): Promise<void> {
    await this.options.renderer.updateCard(messageId, renderSelectionCard(await this.selectionState(view)))
  }

  private async renderExecution(
    messageId: string,
    threadKey: string,
    executionId: string,
    principalId: string,
    afterEventId = 0
  ): Promise<void> {
    const mapper = new CodexAppServerRendererEventMapper({ sessionId: threadKey })
    let status = '正在处理'
    let changesetId: string | undefined
    let lastRender = 0
    let lastEventCursor = 0
    for await (const event of this.options.api.streamEvents(threadKey, executionId, afterEventId)) {
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
      }
      const terminal = Boolean(changesetId)
        || ['development.changeset_empty', 'development.changeset_needs_agent_completion', 'development.changeset_failed'].includes(event.event)
      if (terminal || Date.now() - lastRender >= 1_200) {
        lastRender = Date.now()
        await this.options.renderer.updateCard(messageId, renderProgressCard({
          status,
          answer: mapper.answerText(),
          ...(changesetId ? {
            changesetId,
            consoleUrl: this.changeSetUrl(changesetId, principalId)
          } : {}),
          failed: event.event === 'development.changeset_failed'
        }))
        const delivery = await this.options.api.getDelivery(threadKey)
        await this.recordDelivery(
          threadKey,
          messageId,
          lastEventCursor,
          delivery.desired_version
        )
      }
      if (terminal) {
        const delivery = await this.options.api.getDelivery(threadKey)
        if (delivery.last_event_cursor < lastEventCursor || delivery.state !== 'delivered') {
          await this.recordDelivery(
            threadKey,
            messageId,
            lastEventCursor,
            delivery.desired_version
          )
        }
        return
      }
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

  private async recordDelivery(
    threadKey: string,
    messageId: string,
    lastEventCursor: number,
    desiredVersion: number
  ): Promise<boolean> {
    try {
      await this.options.api.recordDelivery(
        threadKey,
        messageId,
        lastEventCursor,
        desiredVersion
      )
      return true
    } catch (error) {
      if (!(error instanceof FeishuApiError) || error.status !== 409) throw error
      this.metrics.recordStaleConflict('delivery')
      return false
    }
  }

  private async pollPublication(messageId: string, batchId: string, principalId: string): Promise<void> {
    for (let attempt = 0; attempt < 150; attempt += 1) {
      await Bun.sleep(2_000)
      const batch = await this.options.api.getPublishBatch(batchId, principalId) as PublicationBatch
      await this.options.renderer.updateCard(messageId, renderPublicationCard(batch))
      if (['succeeded', 'partially_succeeded', 'failed'].includes(batch.state)) {
        const changeset = await this.options.api.getChangeset(batch.changeset_id, principalId)
        const threadKey = string(record(changeset)?.thread_key)
        if (threadKey) {
          const delivery = await this.options.api.getDelivery(threadKey)
          await this.recordDelivery(
            threadKey,
            messageId,
            delivery.last_event_cursor,
            delivery.desired_version
          )
        }
        return
      }
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
