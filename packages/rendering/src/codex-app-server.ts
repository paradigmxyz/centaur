import type { RustSessionStreamEvent } from '@centaur/harness-events'
import { ChatSDKRenderer, type ChatSDKOutput, type ChatSDKStreamChunk } from './chat-sdk'
import { elementsToPlainText, preformatted as pre, section, text } from './rich-text'
import type {
  RendererEvent,
  RendererLogInfo,
  RendererTaskBlock,
  RendererSourceMapper,
  RendererTaskStatus
} from './types'

const COMMAND_EXECUTION_TITLE = 'Command execution'
const PRE_STREAM_GRACE_MS = 500

const limits = {
  stream: {
    planTitleChars: 256,
    taskTitleChars: 128
  },
  finalPlan: {
    taskTitleChars: 140,
    taskOutputCodeBlockLines: 4,
    outputPreviewChars: 2_200
  }
} as const

type AgentMessagePhase = 'commentary' | 'final_answer'

type ServerNotification = { method?: string; params?: any; type?: string } & Record<string, any>

type HarnessTask = {
  id: string
  title: string
  status: RendererTaskStatus
  details: RendererTaskBlock[]
  output: RendererTaskBlock[]
  commandIndex?: number
}

type CodexMapperState = {
  threadId: string
  stepCounter: number
  nextCommandIndex: number
  lastPlanTitle: string
  answerByItemId: Map<string, string>
  harnessAnswerText: string
  answerText: string
  commentaryByItemId: Map<string, string>
  harnessCommentaryText: string
  commentaryText: string
  completedItemIds: Set<string>
  firstBufferedTextAt: number | null
  streamedCommentaryText: string
  streamedAnswerText: string
  agentMessagePhase: AgentMessagePhase | null
  agentMessagePhaseByItemId: Map<string, AgentMessagePhase>
  planText: string
  taskByUseId: Map<string, HarnessTask>
  commandOutputById: Map<string, string>
  emittedActivityRunByTaskId: Set<string>
  emittedActivityOutputByTaskId: Map<string, string>
  emittedActivitySignatureByTaskId: Map<string, string>
  done: boolean
}

export type CodexAppServerRendererEventMapperOptions = {
  sessionId?: string
  logInfo?: RendererLogInfo
  unknownAgentMessagePhase?: AgentMessagePhase
}

export type CodexAppServerToChatStreamOptions = CodexAppServerRendererEventMapperOptions & {
  onOutput?(output: ChatSDKOutput, event: RendererEvent): Promise<void> | void
  onRendererEvent?(event: RendererEvent): Promise<void> | void
}

export class CodexAppServerRendererEventMapper
  implements RendererSourceMapper<ServerNotification | RustSessionStreamEvent | unknown>
{
  private readonly state: CodexMapperState = newState()
  private readonly sessionId: string
  private readonly logInfo?: RendererLogInfo
  private readonly unknownAgentMessagePhase: AgentMessagePhase

  constructor(options: CodexAppServerRendererEventMapperOptions = {}) {
    this.sessionId = options.sessionId ?? ''
    this.logInfo = options.logInfo
    this.unknownAgentMessagePhase = options.unknownAgentMessagePhase ?? 'final_answer'
  }

  process(source: ServerNotification | RustSessionStreamEvent | unknown): RendererEvent[] {
    if (this.state.done) return []

    const rustMapped = rustSessionEventToServerNotification(source)
    if (rustMapped?.kind === 'failed') return this.fail(rustMapped.error)
    if (rustMapped?.kind === 'completed') return this.flush()
    if (rustMapped?.kind === 'notification') return this.processNotification(rustMapped.notification)

    if (!isRecord(source)) return []
    const notification = normalizeServerNotification(source)
    return notification ? this.processNotification(notification) : []
  }

  flush(): RendererEvent[] {
    if (this.state.done) return []
    this.state.done = true
    const out: RendererEvent[] = []
    completeThinkingTasks(this.state)
    completeOpenTasks(this.state)
    this.emitActivitySummary(out, { final: true })
    this.emitPendingAssistantText(out, { force: true })
    out.push({
      type: 'renderer.done',
      answerMarkdown: this.state.answerText,
      streamFinalUpdates: true,
      threadId: this.state.threadId || undefined
    })
    return out
  }

  isDone(): boolean {
    return this.state.done
  }

  threadId(): string {
    return this.state.threadId
  }

  answerText(): string {
    return this.state.answerText
  }

  private processNotification(rawEvent: ServerNotification): RendererEvent[] {
    const event = normalizeServerNotification(rawEvent)
    if (!event) return []

    const out: RendererEvent[] = []
    if (event?.session_id) this.state.threadId = String(event.session_id)
    if (event?.thread_id) this.state.threadId = String(event.thread_id)
    if (event?.threadId) this.state.threadId = String(event.threadId)

    const error = errorMessage(event)
    if (error) return this.fail(error)

    const title = threadTitleUpdate(event)
    if (title) out.push({ type: 'renderer.title.update', title })

    trackAgentMessageLifecycle(event, this.state)
    ensureCommentarySegmentBreak(event, this.state)
    if (startThinkingTask(this.state, event)) {
      this.emitActivitySummary(out)
    }

    const structuredPlan = structuredPlanUpdate(event)
    if (structuredPlan) {
      const planTitle = structuredPlanTitle(event)
      if (planTitle && planTitle !== this.state.lastPlanTitle) {
        this.state.lastPlanTitle = planTitle
        out.push({ type: 'renderer.plan.update', title: planTitle })
      }
      for (const [index, item] of structuredPlan.entries()) {
        setPlanTask(this.state, index, String(item.step ?? ''), planStatus(item.status))
      }
      this.emitActivitySummary(out)
    }

    const planText = planTextUpdate(event)
    if (planText) {
      this.state.planText =
        event?.type === 'item.plan.delta' ? this.state.planText + planText : planText
      const steps = parsePlanText(this.state.planText)
      for (const [index, item] of steps.entries()) {
        setPlanTask(this.state, index, item.step, item.status)
      }
      this.emitActivitySummary(out)
    }

    const command = commandExecution(event)
    if (command) {
      const id = commandId(command)
      const aggregatedOutput = commandAggregatedOutput(command)
      if (aggregatedOutput) this.state.commandOutputById.set(id, aggregatedOutput)
      const existing = this.state.taskByUseId.get(id)
      const commandIndex = commandNumber(this.state, existing)
      const task = commandTask(
        command,
        String(event?.type ?? ''),
        existing,
        this.state.commandOutputById.get(id),
        commandIndex
      )
      const merged = mergeTask(existing, task)
      this.state.taskByUseId.set(merged.id, merged)
      this.emitActivitySummary(out)
    }

    const fileChange = fileChangeEvent(event)
    if (fileChange) {
      const existing = this.state.taskByUseId.get(fileChangeId(fileChange))
      const task = fileChangeTask(fileChange, String(event?.type ?? ''), existing)
      const merged = mergeTask(existing, task)
      this.state.taskByUseId.set(merged.id, merged)
      this.emitActivitySummary(out)
    }

    const outputDelta = commandOutputDelta(event)
    if (outputDelta) {
      const current = this.state.commandOutputById.get(outputDelta.id) ?? ''
      const output = current + outputDelta.delta
      this.state.commandOutputById.set(outputDelta.id, output)
      const existing = this.state.taskByUseId.get(outputDelta.id)
      const commandIndex = commandNumber(this.state, existing)
      const task =
        existing ??
        ({
          id: outputDelta.id,
          title: commandExecutionTitle(commandIndex),
          status: 'in_progress',
          details: [],
          output: [],
          commandIndex
        } satisfies HarnessTask)
      const updated = {
        ...task,
        title: commandExecutionTitle(commandIndex),
        commandIndex,
        output: commandOutputElements(output)
      }
      this.state.taskByUseId.set(outputDelta.id, updated)
      this.emitActivitySummary(out)
    }

    for (const tool of toolUses(event)) {
      const commandIndex = tool.name === 'Bash' ? commandNumber(this.state) : undefined
      const task: HarnessTask = {
        id: `task-${++this.state.stepCounter}`,
        title: tool.name === 'Bash' ? commandExecutionTitle(commandIndex) : titleFor(tool),
        status: 'in_progress',
        details: detailElementsForTool(tool),
        output: [],
        ...(commandIndex !== undefined ? { commandIndex } : {})
      }
      this.state.taskByUseId.set(String(tool.id), task)
      this.emitActivitySummary(out)
    }

    for (const result of toolResults(event)) {
      const toolUseId = String(result.tool_use_id ?? '')
      const task = this.state.taskByUseId.get(toolUseId) ?? {
        id: `task-${++this.state.stepCounter}`,
        title: 'Tool result',
        status: 'in_progress',
        details: [],
        output: []
      }
      this.state.taskByUseId.set(toolUseId || task.id, task)
      task.status = 'complete'
      task.output = outputElementsForResult(result)
      this.emitActivitySummary(out)
    }

    if (eventCarriesAgentMessageText(event)) {
      const buffer = this.activeAssistantBuffer(event)
      const update = this.applyAgentMessageUpdate(event, buffer)
      if (update.bufferChanged) {
        this.emitPendingAssistantText(out)
      }
      if (update.correction) {
        this.logCanonicalCorrection(event, update.correction)
      }
      if (buffer === 'commentary' && event?.type === 'item.completed') {
        upsertThinkingTask(this.state, event)
        this.emitActivitySummary(out)
      }
    }

    const reasoningMessage = reasoningText(event).trim()
    if (reasoningMessage) {
      const task: HarnessTask = {
        id: `reasoning-${++this.state.stepCounter}`,
        title: 'Thinking',
        status: isReasoningDeltaEvent(event) ? 'in_progress' : 'complete',
        details: [section([text(reasoningMessage)])],
        output: []
      }
      this.state.taskByUseId.set(task.id, task)
      this.emitActivitySummary(out)
    }

    if (isTerminalCodexAppServerEvent(event)) {
      const resultText = terminalResultText(event)
      const willClose = Boolean(resultText || event?.type !== 'result')
      this.logCodexTerminalEventReceived(event, {
        resultText,
        willClose
      })
      if (resultText && !this.state.answerText.trim()) {
        this.state.harnessAnswerText += resultText
        recomposeBuffers(this.state)
        this.emitPendingAssistantText(out, { force: true })
      }
      if (willClose) {
        out.push(...this.flush())
      }
    }

    return out
  }

  private fail(error: string): RendererEvent[] {
    if (this.state.done) return []
    this.state.done = true
    const out: RendererEvent[] = []
    let hadOpenTask = false
    for (const [id, task] of this.state.taskByUseId) {
      if (task.status !== 'in_progress' && task.status !== 'pending') continue
      hadOpenTask = true
      this.state.taskByUseId.set(id, { ...task, status: 'error' })
    }
    if (!this.state.taskByUseId.size || !hadOpenTask) {
      this.state.taskByUseId.set('execution-error', {
        id: 'execution-error',
        title: 'Execution failed',
        status: 'error',
        details: [section([text(error || 'Execution failed')])],
        output: []
      })
    }
    this.emitActivitySummary(out, { final: true })
    this.emitPendingAssistantText(out, { force: true })
    out.push({
      type: 'renderer.done',
      answerMarkdown: this.state.answerText,
      error,
      streamFinalUpdates: true,
      threadId: this.state.threadId || undefined
    })
    return out
  }

  private emitActivitySummary(out: RendererEvent[], opts: { final?: boolean } = {}): void {
    const tasks = Array.from(this.state.taskByUseId.values())
    if (!tasks.length) return
    for (const update of changedActivityTaskUpdates(this.state, tasks, opts)) {
      out.push({
        type: 'renderer.task.update',
        task: {
          id: update.id,
          title: update.title,
          status: update.status,
          details: update.details,
          output: update.output
        },
        flush: true
      })
    }
    this.emitPendingAssistantText(out)
  }

  private emitPendingAssistantText(
    out: RendererEvent[],
    opts: { force?: boolean } = {}
  ): void {
    if (
      this.state.firstBufferedTextAt === null &&
      (this.state.commentaryText.trim() || this.state.answerText.trim())
    ) {
      this.state.firstBufferedTextAt = Date.now()
    }
    this.state.streamedCommentaryText = this.state.commentaryText
    const hasPlan = this.state.taskByUseId.size > 0
    const graceExpired =
      this.state.firstBufferedTextAt !== null &&
      Date.now() - this.state.firstBufferedTextAt >= PRE_STREAM_GRACE_MS
    const canStream = hasPlan || opts.force || graceExpired
    if (!canStream) return

    if (this.state.commentaryText.length > this.state.streamedCommentaryText.length) return
    if (this.state.answerText.length <= this.state.streamedAnswerText.length) return
    const delta = this.state.answerText.slice(this.state.streamedAnswerText.length)
    if (!delta) return
    this.state.streamedAnswerText += delta
    out.push({
      type: 'renderer.message.delta',
      delta,
      force: opts.force ?? false,
      planPrefix: hasPlan
    })
  }

  private activeAssistantBuffer(event: ServerNotification): 'commentary' | 'answer' {
    if (event?.type === 'item.agentMessage.delta' || event?.type === 'item.completed') {
      const codexId = agentMessageEventId(event)
      const itemPhase = this.state.agentMessagePhaseByItemId.get(codexId)
      if (itemPhase) return itemPhase === 'final_answer' ? 'answer' : 'commentary'
      if (
        event?.type === 'item.completed' &&
        (event?.item?.type === 'agentMessage' || event?.item?.type === 'agent_message') &&
        this.state.taskByUseId.size > 0
      ) {
        this.log('codex_renderer_unphased_final_agent_message_classified', {
          agent_session_id: this.sessionId,
          centaur_thread_key: event?.centaur_thread_key,
          execution_id: event?.centaur_execution_id,
          assignment_generation: event?.centaur_assignment_generation,
          codex_id: codexId,
          codex_item_id: codexId,
          codex_item_type: event?.item?.type,
          codex_session_id: this.state.threadId || event?.session_id || event?.thread_id,
          task_count: this.state.taskByUseId.size,
          commentary_chars: this.state.commentaryText.length,
          answer_chars: this.state.answerText.length,
          item_text_chars: String(event?.item?.text ?? '').length
        })
        return 'answer'
      }
      if (codexId) return this.unknownAgentMessagePhase === 'final_answer' ? 'answer' : 'commentary'
      return (this.state.agentMessagePhase ?? this.unknownAgentMessagePhase) === 'final_answer'
        ? 'answer'
        : 'commentary'
    }
    return 'answer'
  }

  private applyAgentMessageUpdate(
    event: ServerNotification,
    buffer: 'answer' | 'commentary'
  ): {
    bufferChanged: boolean
    correction?: { previous: string; canonical: string }
  } {
    const itemId = agentMessageEventId(event)

    if (event?.type === 'item.agentMessage.delta') {
      if (!itemId || this.state.completedItemIds.has(itemId)) return { bufferChanged: false }
      const delta = extractDeltaText(event)
      if (!delta) return { bufferChanged: false }
      const byId = buffer === 'answer' ? this.state.answerByItemId : this.state.commentaryByItemId
      byId.set(itemId, (byId.get(itemId) ?? '') + delta)
      recomposeBuffers(this.state)
      return { bufferChanged: true }
    }

    if (event?.type === 'item.completed') {
      const canonical = String(event?.item?.text ?? '')
      if (!canonical) return { bufferChanged: false }
      if (!itemId) {
        this.log('codex_renderer_item_completed_missing_id', {
          agent_session_id: this.sessionId,
          centaur_thread_key: event?.centaur_thread_key,
          execution_id: event?.centaur_execution_id,
          canonical_text_chars: canonical.length,
          canonical_hash: textHash(canonical)
        })
        return { bufferChanged: false }
      }
      const byId = buffer === 'answer' ? this.state.answerByItemId : this.state.commentaryByItemId
      const previous = byId.get(itemId) ?? ''
      this.state.completedItemIds.add(itemId)
      if (canonical === previous) return { bufferChanged: false }
      byId.set(itemId, canonical)
      recomposeBuffers(this.state)
      return {
        bufferChanged: true,
        correction: previous ? { previous, canonical } : undefined
      }
    }

    if (event?.type === 'assistant') {
      const assistantText = assistantTextFromAssistantEvent(event)
      if (!assistantText) return { bufferChanged: false }
      const key = buffer === 'answer' ? 'harnessAnswerText' : 'harnessCommentaryText'
      const before = this.state[key]
      if (assistantText === before || before.endsWith(assistantText)) return { bufferChanged: false }
      if (assistantEventLooksCanonical(event)) {
        this.state[key] = assistantText
      } else if (assistantText.startsWith(before)) {
        this.state[key] = assistantText
      } else {
        this.state[key] = before + assistantText
      }
      recomposeBuffers(this.state)
      return { bufferChanged: true }
    }

    return { bufferChanged: false }
  }

  private logCanonicalCorrection(
    event: ServerNotification,
    correction: { previous: string; canonical: string }
  ): void {
    const { previous, canonical } = correction
    const charsDiff = canonical.length - previous.length
    this.log('codex_renderer_canonical_answer_correction', {
      agent_session_id: this.sessionId,
      centaur_thread_key: event?.centaur_thread_key,
      execution_id: event?.centaur_execution_id,
      assignment_generation: event?.centaur_assignment_generation,
      event_type: event?.type,
      codex_id: agentMessageEventId(event),
      codex_item_id: agentMessageEventId(event),
      codex_item_type: event?.item?.type,
      codex_item_phase: event?.item?.phase,
      codex_session_id: this.state.threadId || event?.session_id || event?.thread_id,
      delta_total_chars: previous.length,
      canonical_text_chars: canonical.length,
      chars_diff: charsDiff,
      delta_hash: textHash(previous),
      canonical_hash: textHash(canonical)
    })
  }

  private logCodexTerminalEventReceived(
    event: ServerNotification,
    opts: { resultText: string; willClose: boolean }
  ): void {
    this.log('codex_renderer_terminal_event_received', {
      agent_session_id: this.sessionId,
      centaur_thread_key: event?.centaur_thread_key,
      execution_id: event?.centaur_execution_id,
      assignment_generation: event?.centaur_assignment_generation,
      event_type: event?.type,
      codex_session_id: this.state.threadId || event?.session_id || event?.thread_id,
      already_completed: false,
      will_close: opts.willClose,
      result_text_chars: opts.resultText.length,
      answer_chars_before_event: this.state.answerText.length,
      task_count: this.state.taskByUseId.size
    })
  }

  private log(event: string, fields: Record<string, unknown>): void {
    this.logInfo?.(event, fields)
  }
}

export function codexAppServerToRendererEvents(
  sources: Array<ServerNotification | RustSessionStreamEvent | unknown>
): RendererEvent[] {
  const mapper = new CodexAppServerRendererEventMapper()
  return sources.flatMap(source => mapper.process(source))
}

export async function* codexAppServerToChatSdkStream(
  sources: AsyncIterable<ServerNotification | RustSessionStreamEvent | unknown>,
  options: CodexAppServerToChatStreamOptions = {}
): AsyncIterable<ChatSDKStreamChunk> {
  const mapper = new CodexAppServerRendererEventMapper(options)
  const renderer = new ChatSDKRenderer()

  for await (const source of sources) {
    for (const event of mapper.process(source)) {
      yield* renderChatSdkChunks(renderer, mapper.threadId(), event, options)
    }
    if (mapper.isDone()) return
  }

  for (const event of mapper.flush()) {
    yield* renderChatSdkChunks(renderer, mapper.threadId(), event, options)
  }
}

async function* renderChatSdkChunks(
  renderer: ChatSDKRenderer,
  sessionId: string,
  event: RendererEvent,
  options: CodexAppServerToChatStreamOptions
): AsyncIterable<ChatSDKStreamChunk> {
  await options.onRendererEvent?.(event)
  const outputs = renderer.render(sessionId, event)
  for (const output of outputs) {
    await options.onOutput?.(output, event)
    if (output.type !== 'chat.stream.append') continue
    for (const chunk of output.chunks) yield chunk
  }
}

export type RustSessionMappingResult =
  | { kind: 'notification'; notification: ServerNotification }
  | { kind: 'failed'; error: string }
  | { kind: 'completed' }
  | null

export function rustSessionEventToServerNotification(source: unknown): RustSessionMappingResult {
  if (!isRecord(source)) return null
  const eventKind = String(source.eventKind ?? source.event ?? '')
  if (!eventKind.startsWith('session.')) return null

  if (eventKind === 'session.output.line') {
    const data = source.data
    if (isRecord(data)) {
      const notification = normalizeServerNotification(data)
      if (notification) return { kind: 'notification', notification }
    }
    const line = typeof data === 'string' ? data : isRecord(data) ? String(data.raw ?? '') : ''
    const notification = parseServerNotificationLine(line)
    if (notification) return { kind: 'notification', notification }
    return {
      kind: 'notification',
      notification: {
        type: 'command_execution',
        command: 'Session output',
        aggregated_output: line,
        status: 'completed'
      }
    }
  }

  if (
    eventKind === 'session.execution_failed' ||
    eventKind === 'session.stream_error' ||
    eventKind === 'session.stdout_pump_failed'
  ) {
    const data = isRecord(source.data) ? source.data : source
    return { kind: 'failed', error: String(data.error ?? 'Execution failed') }
  }

  if (
    eventKind === 'session.execution_completed' ||
    eventKind === 'session.execution_cancelled'
  ) {
    return { kind: 'completed' }
  }

  return null
}

export function isTerminalCodexAppServerEvent(event: unknown): boolean {
  return (
    isRecord(event) &&
    (event.type === 'result' || event.type === 'turn.done' || event.type === 'turn.completed')
  )
}

function newState(): CodexMapperState {
  return {
    threadId: '',
    stepCounter: 0,
    nextCommandIndex: 0,
    lastPlanTitle: '',
    answerByItemId: new Map(),
    harnessAnswerText: '',
    answerText: '',
    commentaryByItemId: new Map(),
    harnessCommentaryText: '',
    commentaryText: '',
    completedItemIds: new Set(),
    firstBufferedTextAt: null,
    streamedCommentaryText: '',
    streamedAnswerText: '',
    agentMessagePhase: null,
    agentMessagePhaseByItemId: new Map(),
    planText: '',
    taskByUseId: new Map(),
    commandOutputById: new Map(),
    emittedActivityRunByTaskId: new Set(),
    emittedActivityOutputByTaskId: new Map(),
    emittedActivitySignatureByTaskId: new Map(),
    done: false
  }
}

function isRecord(value: unknown): value is Record<string, any> {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value))
}

function normalizeServerNotification(source: unknown): ServerNotification | null {
  if (!isRecord(source)) return null
  if (typeof source.type === 'string') return source as ServerNotification
  if (typeof source.method !== 'string') return null

  const params = isRecord(source.params) ? source.params : {}
  return {
    ...params,
    type: source.method.replace(/\//g, '.')
  }
}

function parseServerNotificationLine(line: string): ServerNotification | null {
  if (!line.trim()) return null
  try {
    const parsed = JSON.parse(line) as unknown
    return normalizeServerNotification(parsed)
  } catch {}
  return null
}

function errorMessage(event: any): string {
  if (String(event?.type ?? '') !== 'error') return ''
  const error = event?.error
  if (typeof error === 'string') return error
  if (isRecord(error) && typeof error.message === 'string') return error.message
  return String(event?.message ?? 'Execution failed')
}

function content(event: any): any[] {
  return Array.isArray(event?.message?.content) ? event.message.content : []
}

function agentMessageItemPhase(item: any): AgentMessagePhase | null {
  const phase = String(item?.phase ?? '').toLowerCase()
  if (phase === 'commentary') return 'commentary'
  if (phase === 'final_answer' || phase === 'finalanswer') return 'final_answer'
  return null
}

function trackAgentMessageLifecycle(event: any, state: CodexMapperState): void {
  if (event?.type !== 'item.started' && event?.type !== 'item.completed') return
  const phase = agentMessageItemPhase(event?.item)
  if (!phase) return
  state.agentMessagePhase = phase
  const id = agentMessageEventId(event)
  if (id) state.agentMessagePhaseByItemId.set(id, phase)
}

function agentMessageEventId(event: any): string {
  return String(event?.itemId ?? event?.item_id ?? event?.item?.id ?? event?.turnId ?? event?.turn_id ?? '')
}

function ensureCommentarySegmentBreak(event: any, state: CodexMapperState): void {
  if (event?.type !== 'item.started') return
  if (agentMessageItemPhase(event?.item) !== 'commentary') return
  const lastId = lastInsertedKey(state.commentaryByItemId)
  if (lastId) {
    const prior = state.commentaryByItemId.get(lastId) ?? ''
    if (prior.trim() && !prior.endsWith('\n\n')) {
      state.commentaryByItemId.set(lastId, prior.endsWith('\n') ? `${prior}\n` : `${prior}\n\n`)
    }
  } else if (state.harnessCommentaryText.trim() && !state.harnessCommentaryText.endsWith('\n\n')) {
    state.harnessCommentaryText = state.harnessCommentaryText.endsWith('\n')
      ? `${state.harnessCommentaryText}\n`
      : `${state.harnessCommentaryText}\n\n`
  } else {
    return
  }
  recomposeBuffers(state)
}

function lastInsertedKey<K>(map: Map<K, unknown>): K | undefined {
  let last: K | undefined
  for (const key of map.keys()) last = key
  return last
}

function commentaryItemId(event: any): string {
  return String(event?.itemId ?? event?.item_id ?? event?.item?.id ?? '')
}

function startThinkingTask(state: CodexMapperState, event: any): boolean {
  if (event?.type !== 'item.started') return false
  if (agentMessageItemPhase(event?.item) !== 'commentary') return false
  const id = commentaryItemId(event)
  if (!id || state.taskByUseId.has(`thinking-${id}`)) return false
  state.taskByUseId.set(`thinking-${id}`, {
    id: `thinking-${id}`,
    title: 'Thinking',
    status: 'in_progress',
    details: [],
    output: []
  })
  return true
}

function upsertThinkingTask(state: CodexMapperState, event: any): void {
  const id = commentaryItemId(event)
  if (!id) return
  const body = String(event?.item?.text ?? state.commentaryByItemId.get(id) ?? '').trim()
  if (!body) return
  if (state.commentaryByItemId.get(id) !== body) {
    state.commentaryByItemId.set(id, body)
    recomposeBuffers(state)
  }
  state.taskByUseId.set(`thinking-${id}`, {
    id: `thinking-${id}`,
    title: 'Thinking',
    status: 'complete',
    details: [section([text(body)])],
    output: []
  })
}

function completeThinkingTasks(state: CodexMapperState): void {
  for (const [id, body] of state.commentaryByItemId) {
    upsertThinkingTask(state, { item: { id, text: body } })
  }
}

function eventCarriesAgentMessageText(event: any): boolean {
  if (event?.type === 'item.agentMessage.delta') return Boolean(extractDeltaText(event))
  if (event?.type === 'assistant') return Boolean(assistantTextFromAssistantEvent(event))
  if (event?.type === 'item.completed') {
    const itemType = event?.item?.type
    if (itemType !== 'agentMessage' && itemType !== 'agent_message') return false
    return Boolean(String(event?.item?.text ?? ''))
  }
  return false
}

function recomposeBuffers(state: CodexMapperState): void {
  state.answerText = compose(state.answerByItemId, state.harnessAnswerText)
  state.commentaryText = compose(state.commentaryByItemId, state.harnessCommentaryText)
}

function compose(byItemId: Map<string, string>, trailing: string): string {
  let out = ''
  for (const value of byItemId.values()) out += value
  return trailing ? out + trailing : out
}

function extractDeltaText(event: any): string {
  const delta = event?.delta ?? event?.text ?? event?.content ?? ''
  if (delta && typeof delta === 'object') return String(delta.text ?? delta.content ?? '')
  return String(delta)
}

function assistantTextFromAssistantEvent(event: any): string {
  return content(event)
    .map(part => (part?.type === 'text' ? (part.text ?? '') : ''))
    .filter(Boolean)
    .join('')
}

function assistantEventLooksCanonical(event: any): boolean {
  const message = event?.message
  return Boolean(
    event?.uuid ||
      event?.request_id ||
      event?.session_id ||
      message?.id ||
      message?.model ||
      message?.usage
  )
}

function textHash(value: string): string {
  let hash = 0x811c9dc5
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index)
    hash = Math.imul(hash, 0x01000193)
  }
  return (hash >>> 0).toString(16).padStart(8, '0')
}

function reasoningText(event: any): string {
  if (
    event?.type === 'item.reasoning.summaryTextDelta' ||
    event?.type === 'item.reasoning.textDelta'
  ) {
    return String(event.delta ?? '')
  }
  if (event?.type !== 'reasoning') return ''
  return String(event.text ?? event.thinking ?? '')
}

function isReasoningDeltaEvent(event: any): boolean {
  return (
    event?.type === 'item.reasoning.summaryTextDelta' ||
    event?.type === 'item.reasoning.textDelta'
  )
}

function terminalResultText(event: any): string {
  for (const key of ['result', 'result_text', 'text', 'final_text']) {
    const value = event?.[key]
    if (typeof value !== 'string') continue
    const resultText = value.trim()
    if (resultText) return resultText
  }
  return ''
}

function toolUses(event: any): any[] {
  if (event?.type !== 'assistant') return []
  return content(event).filter(part => part?.type === 'tool_use')
}

function toolResults(event: any): any[] {
  if (event?.type !== 'user' && event?.type !== 'tool') return []
  const direct = Array.isArray(event?.content) ? event.content : []
  return direct.filter((part: any) => part?.type === 'tool_result' || part?.tool_use_id)
}

function commandExecution(event: any): Record<string, any> | null {
  if (event?.type === 'command_execution') return event
  if (
    event?.type !== 'item.started' &&
    event?.type !== 'item.updated' &&
    event?.type !== 'item.completed'
  )
    return null
  const item = event.item
  if (!item || (item.type !== 'commandExecution' && item.type !== 'command_execution')) return null
  return item
}

function fileChangeEvent(event: any): Record<string, any> | null {
  if (event?.type === 'file_change') return event
  if (
    event?.type !== 'item.started' &&
    event?.type !== 'item.updated' &&
    event?.type !== 'item.completed'
  )
    return null
  const item = event.item
  if (!item || (item.type !== 'fileChange' && item.type !== 'file_change')) return null
  return item
}

function structuredPlanUpdate(event: any): Array<{ step: string; status?: string }> | null {
  if (event?.type !== 'turn.plan.updated') return null
  return Array.isArray(event.plan) ? event.plan : null
}

function structuredPlanTitle(event: any): string {
  return String(event?.explanation ?? event?.title ?? '').trim()
}

function planTextUpdate(event: any): string {
  if (event?.type === 'item.plan.delta') {
    return String(event.delta ?? event.text ?? '')
  }
  if (event?.type === 'item.completed' && event?.item?.type === 'plan') {
    return String(event.item.text ?? '')
  }
  return ''
}

function threadTitleUpdate(event: any): string {
  const eventType = String(event?.type ?? '')
  if (
    eventType !== 'thread/name/updated' &&
    eventType !== 'thread.name.updated' &&
    eventType !== 'thread.name_updated'
  ) {
    return ''
  }
  return String(
    event?.name ?? event?.title ?? event?.threadName ?? event?.thread_name ?? event?.thread?.name ?? ''
  ).trim()
}

function parsePlanText(value: string): Array<{ step: string; status: RendererTaskStatus }> {
  return value
    .split('\n')
    .map(line => {
      const trimmed = line.trim()
      if (!/^[-*]\s+|\d+[.)]\s+/.test(trimmed)) return null
      return {
        step: trimmed,
        status: /\[[xX]\]/.test(trimmed) ? ('complete' as const) : ('pending' as const)
      }
    })
    .filter(item => item !== null)
}

function planStatus(value: string | undefined): RendererTaskStatus {
  const status = String(value ?? '').toLowerCase()
  if (status === 'inprogress' || status === 'in_progress' || status === 'running')
    return 'in_progress'
  if (status === 'completed' || status === 'complete' || status === 'done') return 'complete'
  if (status === 'failed' || status === 'error') return 'complete'
  return 'pending'
}

function stripPlanMarker(value: string): string {
  return value
    .replace(/^\s*(?:[-*]|\d+[.)])\s+/, '')
    .replace(/^\[[ xX]\]\s+/, '')
    .trim()
}

function setPlanTask(
  state: CodexMapperState,
  index: number,
  step: string,
  status: RendererTaskStatus
): void {
  const title = oneLine(stripPlanMarker(step), limits.stream.planTitleChars)
  if (!title) return
  state.taskByUseId.set(`plan-${index + 1}`, {
    id: `plan-${index + 1}`,
    title,
    status,
    details: [],
    output: []
  })
}

function completeOpenTasks(state: CodexMapperState): void {
  for (const [id, task] of state.taskByUseId) {
    if (task.status !== 'in_progress' && task.status !== 'pending') continue
    state.taskByUseId.set(id, { ...task, status: 'complete' })
  }
}

function changedActivityTaskUpdates(
  state: CodexMapperState,
  tasks: HarnessTask[],
  opts: { final?: boolean } = {}
): Array<{
  id: string
  title: string
  status: RendererTaskStatus
  details?: RendererTaskBlock[]
  output?: RendererTaskBlock[]
}> {
  const updates: Array<{
    id: string
    title: string
    status: RendererTaskStatus
    details?: RendererTaskBlock[]
    output?: RendererTaskBlock[]
  }> = []
  for (const task of tasks) {
    let details: RendererTaskBlock[] | undefined
    let output: RendererTaskBlock[] | undefined
    if (task.details.length) {
      const runBlock = activityRunBlock(task)
      if (runBlock.length && !state.emittedActivityRunByTaskId.has(task.id)) {
        state.emittedActivityRunByTaskId.add(task.id)
        details = runBlock
      }
    }
    if (task.output.length) {
      const outputBlock = activityOutputBlock(task)
      const emittedOutput = state.emittedActivityOutputByTaskId.get(task.id) ?? ''
      const currentOutput = elementsToPlainText(outputBlock)
      const outputDelta = currentOutput.startsWith(emittedOutput)
        ? currentOutput.slice(emittedOutput.length)
        : currentOutput
      if (outputDelta) {
        state.emittedActivityOutputByTaskId.set(task.id, currentOutput)
        output = [pre(outputDelta, firstPreformattedLanguage(outputBlock) ?? 'text')]
      }
    }
    const update = {
      id: task.id,
      title: task.title,
      status: task.status,
      details,
      output
    }
    const signature = JSON.stringify(update)
    if (state.emittedActivitySignatureByTaskId.get(task.id) === signature) continue
    state.emittedActivitySignatureByTaskId.set(task.id, signature)
    updates.push(update)
  }
  return updates
}

function activityRunBlock(task: HarnessTask): RendererTaskBlock[] {
  if (task.title === 'Thinking' && task.details.length) {
    return task.details
  }
  const command = firstPreformattedBody(task.details)
  if (command) {
    return [pre(command, shellLanguage(firstPreformattedLanguage(task.details)))]
  }
  return task.details
}

function activityOutputBlock(task: HarnessTask): RendererTaskBlock[] {
  return [pre(elementsToPlainText(task.output), firstPreformattedLanguage(task.output) ?? 'text')]
}

function firstPreformattedBody(elements: RendererTaskBlock[]): string {
  return elements.find(element => element.type === 'code')?.text ?? ''
}

function firstPreformattedLanguage(elements: RendererTaskBlock[]): string | undefined {
  return elements.find(element => element.type === 'code')?.language
}

function shellLanguage(language: string | undefined): string {
  return language === 'bash' || !language ? 'sh' : language
}

function shellLanguageForCommand(_command: string): string {
  return 'sh'
}

function commandOutputDelta(event: any): { id: string; delta: string } | null {
  if (event?.type !== 'item.commandExecution.outputDelta') return null
  const id = String(event.itemId ?? event.item_id ?? '')
  const delta = String(event.delta ?? '')
  return id && delta ? { id, delta } : null
}

function commandId(item: any): string {
  return String(item.id ?? item.itemId ?? item.command_id ?? item.command ?? 'command')
}

function fileChangeId(item: any): string {
  return String(item.id ?? item.itemId ?? item.path ?? 'file-change')
}

function commandNumber(state: CodexMapperState, existing?: HarnessTask): number {
  if (existing?.commandIndex !== undefined) return existing.commandIndex
  state.nextCommandIndex += 1
  return state.nextCommandIndex
}

function commandTask(
  item: any,
  eventType: string,
  existing?: HarnessTask,
  accumulatedOutput?: string,
  commandIndex?: number
): HarnessTask {
  const id = commandId(item)
  const rawCommand = String(item.command ?? 'Command')
  const displayCommand =
    rawCommand === 'Command' ? rawCommand : oneLine(unwrapShellCommand(rawCommand), 220)
  const status = commandStatus(item, eventType)
  const exitCode = item.exitCode ?? item.exit_code
  const failed = isCommandFailure(item, eventType)
  const isCompletionUpdate =
    eventType === 'item.completed' || status === 'complete' || status === 'error'
  const output = commandOutputElements(accumulatedOutput ?? '', exitCode)
  return {
    id,
    title: commandExecutionTitle(commandIndex),
    status,
    ...(commandIndex !== undefined ? { commandIndex } : {}),
    details:
      isCompletionUpdate && existing && !failed
        ? []
        : [pre(displayCommand, shellLanguageForCommand(displayCommand))],
    output
  }
}

function commandAggregatedOutput(item: any): string {
  for (const key of ['aggregated_output', 'aggregatedOutput', 'output', 'stdout', 'stderr']) {
    const value = item?.[key]
    if (typeof value === 'string' && value) return value
  }
  return ''
}

function commandOutputElements(output: string, exitCode?: number | null): RendererTaskBlock[] {
  const elements: RendererTaskBlock[] = []
  const normalizedOutput =
    exitCode !== null && exitCode !== undefined && exitCode !== 0
      ? `exit code ${exitCode}${output ? `\n${output}` : ''}`
      : output
  if (normalizedOutput) {
    const formatted = formatCommandOutput(normalizedOutput)
    elements.push(pre(formatted.body, formatted.language))
  }
  return elements
}

function formatCommandOutput(output: string): { body: string; language: string } {
  const sanitized = sanitizeCommandOutput(output)
  if (sanitized.binary) {
    return { body: sanitized.body, language: 'text' }
  }

  const trimmed = sanitized.body.trim()
  if (trimmed.startsWith('{') || trimmed.startsWith('[')) {
    try {
      const pretty = JSON.stringify(JSON.parse(trimmed), null, 2)
      return {
        body: clipLines(pretty, limits.finalPlan.taskOutputCodeBlockLines),
        language: 'json'
      }
    } catch {}
  }
  return {
    body: clipLines(sanitized.body, limits.finalPlan.taskOutputCodeBlockLines),
    language: languageFromContent(sanitized.body)
  }
}

function sanitizeCommandOutput(output: string): { body: string; binary: boolean } {
  if (!looksLikeBinaryOutput(output)) {
    return {
      body: output.replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/g, '?'),
      binary: false
    }
  }

  const exitCodePrefix = /^exit code \d+\n/.exec(output)?.[0] ?? ''
  return {
    body: `${exitCodePrefix}[binary output omitted; ${output.length - exitCodePrefix.length} chars received]`,
    binary: true
  }
}

function looksLikeBinaryOutput(output: string): boolean {
  const sample = output.slice(0, 4096)
  if (!sample) return false
  if (sample.includes('\u0000')) return true

  let controlChars = 0
  for (let index = 0; index < sample.length; index += 1) {
    const code = sample.charCodeAt(index)
    if ((code < 32 && code !== 9 && code !== 10 && code !== 13) || code === 127) {
      controlChars += 1
    }
  }
  return controlChars >= 8 || controlChars / sample.length > 0.02
}

function fileChangeTask(item: any, eventType: string, existing?: HarnessTask): HarnessTask {
  const id = fileChangeId(item)
  const changes = Array.isArray(item.changes) ? item.changes : []
  const paths = changes.map((change: any) => String(change.path ?? '')).filter(Boolean)
  const uniquePaths: string[] = Array.from(new Set(paths))
  const diff = changes
    .map((change: any) => String(change.diff ?? change.unified_diff ?? '').trim())
    .filter(Boolean)
    .join('\n\n')
  return {
    id,
    title:
      uniquePaths.length === 1
        ? `Edit ${uniquePaths[0]}`
        : uniquePaths.length > 1
          ? `Edit ${uniquePaths.length} files`
          : 'Apply file changes',
    status: itemStatus(item, eventType),
    details: uniquePaths.length
      ? [section([text('Files: '), text(uniquePaths.join(', '), { code: true })])]
      : (existing?.details ?? []),
    output: diff ? [pre(clip(diff), 'diff')] : (existing?.output ?? [])
  }
}

function mergeTask(existing: HarnessTask | undefined, update: HarnessTask): HarnessTask {
  return {
    ...update,
    details: update.details.length ? update.details : (existing?.details ?? []),
    output: update.output.length ? update.output : (existing?.output ?? [])
  }
}

function commandStatus(item: any, eventType: string): RendererTaskStatus {
  if (isCommandFailure(item, eventType)) return 'complete'
  return itemStatus(item, eventType, item.exitCode ?? item.exit_code)
}

function isCommandFailure(item: any, eventType: string): boolean {
  const status = String(item.status ?? '').toLowerCase()
  const exitCode = item.exitCode ?? item.exit_code
  return (
    status === 'failed' ||
    (eventType === 'item.completed' &&
      exitCode !== 0 &&
      exitCode !== null &&
      exitCode !== undefined)
  )
}

function itemStatus(item: any, eventType: string, _exitCode?: number | null): RendererTaskStatus {
  const status = String(item.status ?? '').toLowerCase()
  if (status === 'failed' || status === 'declined') return 'complete'
  if (status === 'completed' || eventType === 'item.completed') {
    return 'complete'
  }
  return 'in_progress'
}

function titleFor(tool: any): string {
  if (tool.name === 'create_file') return 'Create file'
  if (tool.name === 'edit_file') return 'Edit file'
  return `Use ${tool.name ?? 'tool'}`
}

function detailElementsForTool(tool: any): RendererTaskBlock[] {
  if (tool.name === 'Bash') {
    const command = oneLine(unwrapShellCommand(bashCommand(tool.input)), 220)
    return [pre(command, shellLanguageForCommand(command))]
  }
  if (tool.name === 'create_file') {
    const path = stringInput(tool.input, 'path', 'file')
    return [
      section([text('Created '), text(path, { code: true })]),
      pre(stringInput(tool.input, 'content'), languageFromPath(path))
    ]
  }
  if (tool.name === 'edit_file') {
    const path = stringInput(tool.input, 'path', 'file')
    const newStr = stringInput(tool.input, 'new_str')
    const diff = stringInput(tool.input, 'diff')
    const fileContent = stringInput(tool.input, 'content')
    if (newStr)
      return [
        section([text('Edited '), text(path, { code: true })]),
        pre(newStr, languageFromPath(path))
      ]
    if (diff)
      return [section([text('Edited '), text(path, { code: true })]), pre(stripFence(diff), 'diff')]
    if (fileContent)
      return [
        section([text('Edited '), text(path, { code: true })]),
        pre(fileContent, languageFromPath(path))
      ]
    return [section([text('Edited '), text(path, { code: true })])]
  }
  if (tool.name === 'Read') {
    return [
      section([
        text('Read '),
        text(stringInput(tool.input, 'file_path', stringInput(tool.input, 'path', 'file')), {
          code: true
        })
      ])
    ]
  }
  return [pre(JSON.stringify(tool.input ?? {}, null, 2), 'json')]
}

function outputElementsForResult(result: any): RendererTaskBlock[] {
  let raw = result.content ?? ''
  if (Array.isArray(raw))
    raw = raw
      .map((part: any) => (typeof part === 'string' ? part : (part?.text ?? JSON.stringify(part))))
      .join('\n')
  raw = String(raw ?? '')
  try {
    const parsed = JSON.parse(raw) as any
    if (typeof parsed.diff === 'string') return [pre(stripFence(parsed.diff), 'diff')]
    if (parsed.output !== undefined)
      raw =
        typeof parsed.output === 'string' && parsed.output
          ? parsed.output
          : `exitCode ${parsed.exitCode}`
  } catch {}
  const formatted = formatCommandOutput(raw)
  if (formatted.body.includes('\n') || result.is_error) {
    return [pre(formatted.body, formatted.language)]
  }
  return [section([text(oneLine(raw || 'Done'))])]
}

function stripFence(value: string): string {
  return value
    .trim()
    .replace(/^```[a-zA-Z0-9_-]*\n?/, '')
    .replace(/\n?```$/, '')
}

function bashCommand(input: any): string {
  return stringInput(input, 'command', stringInput(input, 'cmd'))
}

function stringInput(input: any, key: string, fallback = ''): string {
  const value = input?.[key]
  return typeof value === 'string' ? value : fallback
}

function languageFromPath(path: string): string {
  const name = path.split('/').pop() ?? ''
  const extension = name.includes('.') ? name.split('.').pop() : ''
  return extension?.toLowerCase() || 'text'
}

function languageFromContent(value: string): string {
  const trimmed = value.trim()
  if (
    /^(export\s+)?(async\s+)?function\s|^type\s+\w+\s*=|^interface\s+\w+|^const\s+\w+\s*[:=]/m.test(
      trimmed
    )
  )
    return 'ts'
  if (trimmed.startsWith('{') || trimmed.startsWith('[')) return 'json'
  return 'text'
}

function clip(value: string, max: number = limits.finalPlan.outputPreviewChars): string {
  return value.length > max ? `${value.slice(0, max)}\n/* truncated */` : value
}

function clipLines(value: string, maxLines: number): string {
  if (maxLines <= 0) return ''
  const lines = value.split('\n')
  if (lines.length <= maxLines) return value
  if (maxLines === 1) return '// truncated'
  return [...lines.slice(0, maxLines - 1), '// truncated'].join('\n')
}

function oneLine(value: string, max: number = limits.finalPlan.taskTitleChars): string {
  const normalized = value.replace(/\s+/g, ' ').trim()
  return normalized.length > max ? `${normalized.slice(0, max - 3)}...` : normalized
}

function unwrapShellCommand(command: string): string {
  const trimmed = command.trim()
  if (!trimmed) return trimmed

  const bashLc = /^\/bin\/bash\s+-lc\s+([\s\S]+)$/i.exec(trimmed)
  if (!bashLc?.[1]) return trimmed

  let inner = bashLc[1].trim()
  if (
    (inner.startsWith("'") && inner.endsWith("'")) ||
    (inner.startsWith('"') && inner.endsWith('"'))
  ) {
    inner = inner.slice(1, -1)
  }
  return inner.trim() || trimmed
}

function commandExecutionTitle(index?: number): string {
  return index !== undefined ? `${index}. ${COMMAND_EXECUTION_TITLE}` : COMMAND_EXECUTION_TITLE
}
