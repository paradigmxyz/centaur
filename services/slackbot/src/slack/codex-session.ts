import type { WebClient } from '@slack/web-api'
import { slackReplyLimits } from '../constants'
import { AgentSessionRenderer } from './agent-session'
import {
  clipLines,
  preformatted as pre,
  richText,
  section,
  text,
  type StreamRichText,
  type StreamRichTextElement
} from './streaming'

const COMMAND_EXECUTION_TITLE = 'Command execution'

type AgentMessagePhase = 'commentary' | 'final_answer'

type HarnessTask = {
  id: string
  title: string
  status: 'pending' | 'in_progress' | 'complete' | 'error'
  details: StreamRichTextElement[]
  output: StreamRichTextElement[]
  commandIndex?: number
}

type CodexSessionState = {
  threadId: string
  stepCounter: number
  nextCommandIndex: number
  commentaryText: string
  commentaryByItemId: Map<string, string>
  answerText: string
  firstBufferedTextAt: number | null
  streamedCommentaryText: string
  streamedAnswerText: string
  deliveredAnswerChars: number
  agentMessagePhase: AgentMessagePhase | null
  planText: string
  taskByUseId: Map<string, HarnessTask>
  commandOutputById: Map<string, string>
  emittedActivityRunByTaskId: Set<string>
  emittedActivityOutputByTaskId: Set<string>
  done: boolean
}

const states = new Map<string, CodexSessionState>()
const PRE_STREAM_GRACE_MS = 500

export class CodexSessionRenderer {
  private readonly renderer: AgentSessionRenderer

  constructor(client: WebClient) {
    this.renderer = new AgentSessionRenderer(client)
  }

  async event(
    agentSessionId: string,
    event: any
  ): Promise<{ threadId?: string; done: boolean; streamedAnswerChars: number }> {
    const state = getState(agentSessionId)
    if (event?.session_id) state.threadId = String(event.session_id)
    if (event?.thread_id) state.threadId = String(event.thread_id)

    trackAgentMessageLifecycle(event, state)
    ensureCommentarySegmentBreak(event, state)

    const structuredPlan = structuredPlanUpdate(event)
    if (structuredPlan) {
      await this.publishStructuredPlan(agentSessionId, state, structuredPlan)
    }

    const planText = planTextUpdate(event)
    if (planText) {
      state.planText = event?.type === 'item.plan.delta' ? state.planText + planText : planText
      await this.publishPlanText(agentSessionId, state, state.planText)
    }

    const command = commandExecution(event)
    if (command) {
      const id = commandId(command)
      const aggregatedOutput = commandAggregatedOutput(command)
      if (aggregatedOutput) state.commandOutputById.set(id, aggregatedOutput)
      const existing = state.taskByUseId.get(id)
      const commandIndex = commandNumber(state, existing)
      const task = commandTask(
        command,
        event?.type,
        existing,
        state.commandOutputById.get(id),
        commandIndex
      )
      const merged = mergeTask(existing, task)
      state.taskByUseId.set(merged.id, merged)
      await this.publishActivitySummary(agentSessionId, state)
    }

    const fileChange = fileChangeEvent(event)
    if (fileChange) {
      const existing = state.taskByUseId.get(fileChangeId(fileChange))
      const task = fileChangeTask(fileChange, event?.type, existing)
      const merged = mergeTask(existing, task)
      state.taskByUseId.set(merged.id, merged)
      await this.publishActivitySummary(agentSessionId, state)
    }

    const outputDelta = commandOutputDelta(event)
    if (outputDelta) {
      const current = state.commandOutputById.get(outputDelta.id) ?? ''
      const output = current + outputDelta.delta
      state.commandOutputById.set(outputDelta.id, output)
      const existing = state.taskByUseId.get(outputDelta.id)
      const commandIndex = commandNumber(state, existing)
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
      state.taskByUseId.set(outputDelta.id, updated)
      await this.publishActivitySummary(agentSessionId, state)
    }

    for (const tool of toolUses(event)) {
      const commandIndex = tool.name === 'Bash' ? commandNumber(state) : undefined
      const task: HarnessTask = {
        id: `task-${++state.stepCounter}`,
        title: tool.name === 'Bash' ? commandExecutionTitle(commandIndex) : titleFor(tool),
        status: 'in_progress',
        details: detailElementsForTool(tool),
        output: [],
        ...(commandIndex !== undefined ? { commandIndex } : {})
      }
      state.taskByUseId.set(String(tool.id), task)
      await this.publishActivitySummary(agentSessionId, state)
    }

    for (const result of toolResults(event)) {
      const toolUseId = String(result.tool_use_id ?? '')
      const task = state.taskByUseId.get(toolUseId) ?? {
        id: `task-${++state.stepCounter}`,
        title: 'Tool result',
        status: 'in_progress',
        details: [],
        output: []
      }
      state.taskByUseId.set(toolUseId || task.id, task)
      task.status = 'complete'
      task.output = outputElementsForResult(result)
      await this.publishActivitySummary(agentSessionId, state)
    }

    const assistantMessage = assistantText(event)
    if (assistantMessage) {
      const buffer = activeAssistantBuffer(state, event)
      const current = buffer === 'answer' ? state.answerText : state.commentaryText
      const delta = messageDelta(current, assistantMessage)
      if (delta) {
        if (buffer === 'answer') state.answerText += delta
        else {
          state.commentaryText += delta
          trackCommentaryText(state, event, delta)
        }
        await this.publishPendingAssistantText(agentSessionId, state)
      }
      if (buffer === 'commentary' && event?.type === 'item.completed') {
        upsertThinkingTask(state, event)
        await this.publishActivitySummary(agentSessionId, state)
      }
    }

    const reasoningMessage = reasoningText(event).trim()
    if (reasoningMessage) {
      const task: HarnessTask = {
        id: `reasoning-${++state.stepCounter}`,
        title: 'Thinking',
        status: 'complete',
        details: [section([text(reasoningMessage)])],
        output: []
      }
      state.taskByUseId.set(task.id, task)
      await this.publishActivitySummary(agentSessionId, state)
    }

    if (isTerminalTurnEvent(event)) {
      if (typeof event.result === 'string' && !state.answerText.trim()) {
        const resultText = event.result.trim()
        if (resultText) {
          state.answerText += resultText
          await this.publishPendingAssistantText(agentSessionId, state, { force: true })
        }
      }
      await this.done(agentSessionId)
    }

    return {
      threadId: state.threadId || undefined,
      done: state.done,
      streamedAnswerChars: state.deliveredAnswerChars
    }
  }

  async done(agentSessionId: string, threadId?: string): Promise<void> {
    const state = getState(agentSessionId)
    if (state.done) return
    if (threadId) state.threadId = threadId
    state.done = true
    completeThinkingTasks(state)
    completeOpenTasks(state)
    await this.publishActivitySummary(agentSessionId, state, { final: true })
    await this.publishPendingAssistantText(agentSessionId, state, { force: true })
    const { streamedTextChars } = await this.renderer.done(agentSessionId, {
      streamFinalUpdates: true,
      commentaryMarkdown: state.commentaryText,
      answerMarkdown: state.answerText
    })
    state.deliveredAnswerChars = streamedTextChars
    state.done = true
    states.delete(agentSessionId)
  }

  private async publishActivitySummary(
    agentSessionId: string,
    state: CodexSessionState,
    opts: { final?: boolean } = {}
  ): Promise<void> {
    const tasks = Array.from(state.taskByUseId.values())
    if (!tasks.length) return
    for (const update of changedActivityTaskUpdates(state, tasks, opts)) {
      await this.renderer.step(
        agentSessionId,
        {
          id: update.id,
          title: update.title,
          status: update.status,
          details: update.details,
          output: update.output
        },
        { flush: true }
      )
    }
    await this.publishPendingAssistantText(agentSessionId, state)
  }

  private async publishPendingAssistantText(
    agentSessionId: string,
    state: CodexSessionState,
    opts: { force?: boolean } = {}
  ): Promise<void> {
    if (
      state.firstBufferedTextAt === null &&
      (state.commentaryText.trim() || state.answerText.trim())
    ) {
      state.firstBufferedTextAt = Date.now()
    }
    state.streamedCommentaryText = state.commentaryText
    const hasPlan = state.taskByUseId.size > 0
    const graceExpired =
      state.firstBufferedTextAt !== null &&
      Date.now() - state.firstBufferedTextAt >= PRE_STREAM_GRACE_MS
    const canStream = hasPlan || opts.force || graceExpired
    if (!canStream) return

    if (state.commentaryText.length > state.streamedCommentaryText.length) return
    if (state.answerText.length <= state.streamedAnswerText.length) return
    const delta = state.answerText.slice(state.streamedAnswerText.length)
    if (!delta) return
    const acceptedChars = await this.renderer.textDelta(agentSessionId, delta, {
      force: opts.force ?? false,
      planPrefix: hasPlan
    })
    if (acceptedChars > 0) {
      state.streamedAnswerText += delta.slice(0, acceptedChars)
      state.deliveredAnswerChars = this.renderer.streamedTextChars(agentSessionId)
    }
  }

  private async publishStructuredPlan(
    agentSessionId: string,
    state: CodexSessionState,
    plan: Array<{ step: string; status?: string }>
  ): Promise<void> {
    for (const [index, item] of plan.entries()) {
      setPlanTask(state, index, String(item.step ?? ''), planStatus(item.status))
    }
    await this.publishActivitySummary(agentSessionId, state)
  }

  private async publishPlanText(
    agentSessionId: string,
    state: CodexSessionState,
    value: string
  ): Promise<void> {
    const steps = parsePlanText(value)
    if (!steps.length) return
    for (const [index, item] of steps.entries()) {
      setPlanTask(state, index, item.step, item.status)
    }
    await this.publishActivitySummary(agentSessionId, state)
  }
}

export function hasActiveCodexSession(agentSessionId: string): boolean {
  const state = states.get(agentSessionId)
  return Boolean(state && !state.done)
}

function getState(agentSessionId: string): CodexSessionState {
  let state = states.get(agentSessionId)
  if (!state) {
    state = {
      threadId: '',
      stepCounter: 0,
      nextCommandIndex: 0,
      commentaryText: '',
      commentaryByItemId: new Map(),
      answerText: '',
      firstBufferedTextAt: null,
      streamedCommentaryText: '',
      streamedAnswerText: '',
      deliveredAnswerChars: 0,
      agentMessagePhase: null,
      planText: '',
      taskByUseId: new Map(),
      commandOutputById: new Map(),
      emittedActivityRunByTaskId: new Set(),
      emittedActivityOutputByTaskId: new Set(),
      done: false
    }
    states.set(agentSessionId, state)
  }
  return state
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

function trackAgentMessageLifecycle(event: any, state: CodexSessionState): void {
  if (event?.type !== 'item.started' && event?.type !== 'item.completed') return
  const phase = agentMessageItemPhase(event?.item)
  if (!phase) return
  state.agentMessagePhase = phase
}

/** Codex may emit several commentary agentMessages in one turn; keep a blank line between them. */
function ensureCommentarySegmentBreak(event: any, state: CodexSessionState): void {
  if (event?.type !== 'item.started') return
  if (agentMessageItemPhase(event?.item) !== 'commentary') return
  const current = state.commentaryText
  if (!current.trim() || current.endsWith('\n\n')) return
  state.commentaryText = current.endsWith('\n') ? `${current}\n` : `${current}\n\n`
}

function commentaryItemId(event: any): string {
  return String(event?.itemId ?? event?.item_id ?? event?.item?.id ?? '')
}

function trackCommentaryText(state: CodexSessionState, event: any, delta: string): void {
  const id = commentaryItemId(event)
  if (!id) return
  state.commentaryByItemId.set(id, `${state.commentaryByItemId.get(id) ?? ''}${delta}`)
}

function upsertThinkingTask(state: CodexSessionState, event: any): void {
  const id = commentaryItemId(event)
  if (!id) return
  const body = String(event?.item?.text ?? state.commentaryByItemId.get(id) ?? '').trim()
  if (!body) return
  const taskId = `thinking-${id}`
  state.commentaryByItemId.set(id, body)
  state.taskByUseId.set(taskId, {
    id: taskId,
    title: 'Thinking',
    status: 'complete',
    details: [section([text(body)])],
    output: []
  })
}

function completeThinkingTasks(state: CodexSessionState): void {
  for (const [id, body] of state.commentaryByItemId) {
    upsertThinkingTask(state, { item: { id, text: body } })
  }
}

function activeAssistantBuffer(state: CodexSessionState, event: any): 'commentary' | 'answer' {
  if (event?.type === 'item.agentMessage.delta' || event?.type === 'item.completed') {
    return state.agentMessagePhase === 'final_answer' ? 'answer' : 'commentary'
  }
  return 'answer'
}

function assistantText(event: any): string {
  if (event?.type === 'item.agentMessage.delta') {
    const delta = event.delta ?? event.text ?? event.content ?? ''
    if (delta && typeof delta === 'object') {
      return String(delta.text ?? delta.content ?? '')
    }
    return String(delta)
  }
  if (
    event?.type === 'item.completed' &&
    (event?.item?.type === 'agentMessage' || event?.item?.type === 'agent_message')
  ) {
    return String(event.item.text ?? '')
  }
  if (event?.type !== 'assistant') return ''
  return content(event)
    .map(part => (part?.type === 'text' ? (part.text ?? '') : ''))
    .filter(Boolean)
    .join('')
}

function messageDelta(current: string, incoming: string): string {
  if (!current) return incoming
  if (incoming.startsWith(current)) return incoming.slice(current.length)
  if (current.endsWith(incoming)) return ''
  return incoming
}

function reasoningText(event: any): string {
  if (event?.type !== 'reasoning') return ''
  return String(event.text ?? event.thinking ?? '')
}

function isTerminalTurnEvent(event: any): boolean {
  return event?.type === 'result' || event?.type === 'turn.done' || event?.type === 'turn.completed'
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

function planTextUpdate(event: any): string {
  if (event?.type === 'item.plan.delta') {
    return String(event.delta ?? event.text ?? '')
  }
  if (event?.type === 'item.completed' && event?.item?.type === 'plan') {
    return String(event.item.text ?? '')
  }
  return ''
}

function parsePlanText(value: string): Array<{ step: string; status: HarnessTask['status'] }> {
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

function planStatus(value: string | undefined): HarnessTask['status'] {
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
  state: CodexSessionState,
  index: number,
  step: string,
  status: HarnessTask['status']
): void {
  const title = oneLine(stripPlanMarker(step), slackReplyLimits.stream.planTitleChars)
  if (!title) return
  state.taskByUseId.set(`plan-${index + 1}`, {
    id: `plan-${index + 1}`,
    title,
    status,
    details: [],
    output: []
  })
}

function completeOpenTasks(state: CodexSessionState): void {
  for (const [id, task] of state.taskByUseId) {
    if (task.status !== 'in_progress' && task.status !== 'pending') continue
    state.taskByUseId.set(id, { ...task, status: 'complete' })
  }
}

function changedActivityTaskUpdates(
  state: CodexSessionState,
  tasks: HarnessTask[],
  opts: { final?: boolean } = {}
): Array<{
  id: string
  title: string
  status: HarnessTask['status']
  details?: StreamRichText
  output?: StreamRichText
}> {
  const updates: Array<{
    id: string
    title: string
    status: HarnessTask['status']
    details?: StreamRichText
    output?: StreamRichText
  }> = []
  for (const task of tasks) {
    let details: StreamRichText | undefined
    let output: StreamRichText | undefined
    if (opts.final) {
      if (task.details.length && !state.emittedActivityRunByTaskId.has(task.id)) {
        state.emittedActivityRunByTaskId.add(task.id)
        details = activityRunBlock(task)
      }
      if (task.output.length && !state.emittedActivityOutputByTaskId.has(task.id)) {
        state.emittedActivityOutputByTaskId.add(task.id)
        output = activityOutputBlock(task)
      }
    } else if (task.details.length && !state.emittedActivityRunByTaskId.has(task.id)) {
      state.emittedActivityRunByTaskId.add(task.id)
      details = activityRunBlock(task)
    }
    if (
      !opts.final &&
      task.status === 'complete' &&
      task.output.length &&
      !state.emittedActivityOutputByTaskId.has(task.id)
    ) {
      state.emittedActivityOutputByTaskId.add(task.id)
      output = activityOutputBlock(task)
    }
    if (!details && !output && !opts.final) continue
    updates.push({
      id: task.id,
      title: task.title,
      status: task.status,
      details,
      output
    })
  }
  return updates
}

function activityRunBlock(task: HarnessTask): StreamRichText {
  if (task.title === 'Thinking' && task.details.length) {
    return richText(task.details)
  }
  const command = firstPreformattedBody(task.details)
  if (command) {
    return richText([pre(command, shellLanguage(firstPreformattedLanguage(task.details)))])
  }
  return richText(task.details)
}

function activityOutputBlock(task: HarnessTask): StreamRichText {
  return richText([
    pre(elementsToPlainText(task.output), firstPreformattedLanguage(task.output) ?? 'text')
  ])
}

function firstPreformattedBody(elements: StreamRichTextElement[]): string {
  return (
    elements
      .find(element => element.type === 'rich_text_preformatted')
      ?.elements.map(inline => inline.text ?? '')
      .join('') ?? ''
  )
}

function firstPreformattedLanguage(elements: StreamRichTextElement[]): string | undefined {
  return elements.find(element => element.type === 'rich_text_preformatted')?.language
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

function commandNumber(state: CodexSessionState, existing?: HarnessTask): number {
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

function commandOutputElements(output: string, exitCode?: number | null): StreamRichTextElement[] {
  const elements: StreamRichTextElement[] = []
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
  const trimmed = output.trim()
  if (trimmed.startsWith('{') || trimmed.startsWith('[')) {
    try {
      const pretty = JSON.stringify(JSON.parse(trimmed), null, 2)
      return {
        body: clipLines(pretty, slackReplyLimits.finalPlan.taskOutputCodeBlockLines),
        language: 'json'
      }
    } catch {}
  }
  return {
    body: clipLines(output, slackReplyLimits.finalPlan.taskOutputCodeBlockLines),
    language: languageFromContent(output)
  }
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

function commandStatus(item: any, eventType: string): HarnessTask['status'] {
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

function itemStatus(
  item: any,
  eventType: string,
  _exitCode?: number | null
): HarnessTask['status'] {
  const status = String(item.status ?? '').toLowerCase()
  if (status === 'failed' || status === 'declined') return 'complete'
  if (status === 'completed' || eventType === 'item.completed') {
    return 'complete'
  }
  return 'in_progress'
}

function elementsToPlainText(elements: StreamRichTextElement[]): string {
  return elements.map(elementToPlainText).filter(Boolean).join('\n')
}

function elementToPlainText(element: StreamRichTextElement): string {
  if (element.type === 'rich_text_preformatted') {
    const body = element.elements?.map(inline => inline.text ?? '').join('') ?? ''
    return body
  }
  if (element.type === 'rich_text_section') {
    return (element.elements ?? [])
      .map(inline => {
        if ('url' in inline) return inline.text ?? inline.url
        if ('user_id' in inline) return `<@${inline.user_id}>`
        return inline.text ?? ''
      })
      .join('')
  }
  return ''
}

function titleFor(tool: any): string {
  if (tool.name === 'create_file') return 'Create file'
  if (tool.name === 'edit_file') return 'Edit file'
  return `Use ${tool.name ?? 'tool'}`
}

function detailElementsForTool(tool: any): StreamRichTextElement[] {
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

function outputElementsForResult(result: any): StreamRichTextElement[] {
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

function clip(value: string, max: number = slackReplyLimits.finalPlan.outputPreviewChars): string {
  return value.length > max ? `${value.slice(0, max)}\n/* truncated */` : value
}

function oneLine(value: string, max: number = slackReplyLimits.finalPlan.taskTitleChars): string {
  const normalized = value.replace(/\s+/g, ' ').trim()
  return normalized.length > max ? `${normalized.slice(0, max - 1)}…` : normalized
}

/** Strip harness wrappers like `/bin/bash -lc 'call tools'`. */
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
